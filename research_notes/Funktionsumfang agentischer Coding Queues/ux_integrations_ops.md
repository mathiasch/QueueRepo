# UX, Integrations and Operations of Top-Tier Agentic Coding Task Queues (as of Nov 2026)

Research scope: what best-in-class background/cloud coding-agent products (Claude Code cloud sessions, OpenAI Codex cloud/app, GitHub Copilot coding agent, Cursor Cloud Agents, Google Jules, Devin, Vibe Kanban, Conductor, Linear agents) offer in intake, review, integrations, mobile, analytics, onboarding, and self-hosted operations. The goal is to compare against "Queue" (self-hosted, German UI, per-user sequential queue, CLI agents in Podman sandboxes, SSE/Web Push/ntfy, git per project, no PR workflow, no integrations, no backups/metrics/admin console).

Method note: Official docs at code.claude.com were fetched in full and are the strongest sources here. These hosts could not be reached from the research sandbox (DNS blocked): developers.openai.com, jules.google, infoq.com, codex.danielvaughan.com. Codex, Jules and Cursor details therefore come mostly from search snippets of official changelogs and third-party guides, and are marked as such. The report writer should treat search-snippet-only claims as less certain.

## 1. Task intake & steering (templates, attachments, scheduling, steering, plan review, priorities)

### Takeaway
The leading products treat a task as a conversation you can keep steering, not a fire-and-forget job. You can queue follow-ups while it runs and take them back, answer the agent's questions later, switch permission or plan modes mid-run, and send messages from a CLI or CI. Scheduling has become a first-class feature (cron, one-off "run at", API and event triggers). Queue already has reordering, but it lacks steering of a running task, plan-before-execute, scheduled or recurring tasks, and triggers.

### Cited Findings
- **Steering a running session:** in Claude Code cloud sessions, a message you send while Claude is working queues until Claude reads it. You can click ✕ on a queued message to take it back into the input box. Once read, it stays. — [Claude Code docs: cloud](https://code.claude.com/docs/en/claude-code-on-the-web)
- **Async answers:** if Claude asks a question and the session sits idle, you can answer later, up to environment expiry, and the session continues from your answer. — [Claude Code docs: cloud](https://code.claude.com/docs/en/claude-code-on-the-web)
- **Follow-ups from CLI and CI:** `claude -p "msg" --cloud <session-id>` queues a message into a running cloud session from any logged-in machine and exits. It is meant "to steer a long-running session, queue the next step while the current one is still finishing, or send follow-ups from a CI script". `--output-format json` returns `{ok, session_id, url}`. — [Claude Code docs: cloud](https://code.claude.com/docs/en/claude-code-on-the-web)
- **Messages queued during provisioning:** while the cloud container starts, the CLI shows a live checklist of setup steps (clone, setup script) and queues messages typed meanwhile. — [Claude Code docs: cloud](https://code.claude.com/docs/en/claude-code-on-the-web)
- **Plan review before execution:** the permission mode (Accept edits / Plan / Auto) can be picked at task creation and changed while the session runs. The recommended pattern is "plan locally, execute in the cloud": plan mode explores without editing, then the plan is committed and a cloud session executes it. — [Claude Code docs: cloud](https://code.claude.com/docs/en/claude-code-on-the-web); [Claude Code docs: mobile](https://code.claude.com/docs/en/mobile)
- **Jules plan approval (secondary source):** per a third-party write-up, Jules API plans default to automatic approval unless `requirePlanApproval` is true, and the web UI auto-approves a plan on a timer if the user leaves. The REST API is v1alpha. — [rywalker.com research on Jules](https://rywalker.com/research/jules)
- **Rewind:** Claude Code cloud has a "Rewind to here" action on earlier messages (mentioned in troubleshooting for self-hosted runners). — [Claude Code docs: cloud](https://code.claude.com/docs/en/claude-code-on-the-web)
- **Scheduling and recurring tasks (Claude Code "scheduled tasks", formerly routines, research preview):** a saved prompt plus repos plus connectors, with one or more triggers:
  - Schedule: hourly, nightly, weekly, or a one-off at a future time. Custom cron is allowed, but nothing more frequent than hourly; `CRON_TZ=` prefixes are supported.
  - API: POST to `/fire` with a bearer token and an optional free-text `text` field (for example an alert body). It returns a session URL.
  - GitHub events, with PR filters on base/head branch, labels and title regex.
  - Limits: 100 scheduled runs/hour per account, and 30/hour per task for Run-now and API fires.
  - The run history shows Succeeded/Failed/Cancelled. The docs note this is the session outcome, "not whether Claude completed the work".
  - Runs push to `claude/`-prefixed branches. If GitHub auth lapses, runs are skipped for up to 72 h before the task turns off.

  — [Claude Code docs: scheduled tasks](https://code.claude.com/docs/en/routines)
- **Example recurring use cases from those docs:**
  - Nightly backlog grooming: read new issues, label them, assign owners, post a Slack summary.
  - Deploy verification triggered by the CD pipeline.
  - Porting merged PRs to a sibling SDK.

  — [Claude Code docs: scheduled tasks](https://code.claude.com/docs/en/routines)
- **Cursor Automations:** start cloud agents without a person, on a schedule or on events from source control, Slack, a webhook, Linear, Sentry or PagerDuty (third-party guide). — [developertoolkit.ai](https://developertoolkit.ai/en/cursor-ide/advanced-techniques/cloud-agents-automations/); [spotdev](https://www.spotdev.co.uk/blog/cursor-cloud-agents-and-automations)
- **Jules scheduled tasks:** Jules docs navigation lists "Scheduled tasks" and "Jules Tools (CLI)" sections, but the content could not be fetched. — search result for [jules.google docs](https://jules.google/docs/contact)
- **Attachments from mobile:** in the Claude app, attached photos are seen directly by Claude, and also saved under `~/.claude/uploads/` so Claude can copy them into files. Other files are passed as `@` file references. — [Claude Code docs: mobile](https://code.claude.com/docs/en/mobile)
- **Voice:** OpenAI's ChatGPT/Codex changelog mentions voice continuing in the background and keeping task actions going (search snippet only). — [ChatGPT & Codex changelog](https://learn.chatgpt.com/docs/changelog)
- **Multiple chats per workspace:** Conductor lets you put multiple chats in one workspace when work needs shared code state, or one workspace (branch + worktree) per task. — [Conductor docs](https://www.conductor.build/docs/guides/parallel-agents/run-multiple-claude-code-sessions)
- **Projects / orchestration:** Claude Code "projects" are one conversation where Claude starts and tracks many parallel cloud sessions for a body of work and reports back which threads finished or need you. — [Claude Code docs: mobile](https://code.claude.com/docs/en/mobile); [Claude Code docs: cloud](https://code.claude.com/docs/en/claude-code-on-the-web)

### Inferences
- For Queue the biggest steering gaps are:
  - (a) queuing a follow-up message into a *running* task, with a cancel/undo for not-yet-read messages
  - (b) a "plan first, then approve" mode per task, with an optional auto-approve timeout like Jules
  - (c) scheduled tasks: cron, "run at night when quota resets", one-off "run at", and an HTTP fire endpoint with a token
  - (d) answering agent questions asynchronously, which fits the existing "needs-you" lane
- "Run at night when quota resets" is not offered explicitly by anyone found. A quota-aware scheduler would be a differentiator for a sequential, quota-bound queue.
- Mirror the scheduled-task outcome semantics: show session success separately from "work actually done". Queue's review step could provide the second signal.

### Gaps
- No source found for task templates/recipes, task dependencies (task B after A), priorities/labels or bulk actions in these products. They may exist (for example via Linear/Jira as the tracker), but they were not confirmed.
- No reliable source for @-mentions of files in web composers. It exists in CLIs (`@` file references), but web composer behavior was not confirmed.
- Codex cloud official docs (developers.openai.com) were unreachable. Codex features like best-of-N attempts per task could not be verified there.

## 2. Review UX (PRs, PR-comment iteration, inline diff comments, attempt comparison, previews, screenshots, test reports)

### Takeaway
The standard pattern is "agent opens a draft PR, humans comment, agent iterates". It now extends to auto-fixing CI failures and review comments, and to inline diff comments in the agent's own UI that are sent back as the next message. Parallel attempts compared side by side, and live app previews, are the newer differentiators. Queue's biggest review gap is the missing PR workflow.

### Cited Findings
- **GitHub Copilot coding agent: assignment.** You assign an issue to Copilot like a person. It reacts 👀, then opens a linked draft PR. — [GitHub Docs: using Copilot coding agent](https://docs.github.com/en/enterprise-cloud@latest/early-access/copilot/coding-agent/using-copilot-coding-agent); [GitHub blog 101](https://github.blog/ai-and-ml/github-copilot/github-copilot-coding-agent-101-getting-started-with-agentic-workflows-on-github/)
- **GitHub Copilot coding agent: iteration and limits.** When it finishes, it requests review. Comments mentioning @copilot on the PR drive iterations. Copilot cannot mark its PR ready for review, approve or merge, and comments from users without write access are never shown to the agent. — [GitHub Docs: about coding agent](https://docs.github.com/ko/enterprise-cloud@latest/copilot/concepts/coding-agent/about-copilot-coding-agent)
- **GitHub Copilot coding agent: Jira and speed (Mar 2026 changelogs).** Jira issues can be assigned to the agent (public preview), and work starts 50% faster. — [GitHub Changelog: Jira](https://github.blog/changelog/2026-03-05-github-copilot-coding-agent-for-jira-is-now-in-public-preview/); [GitHub Changelog: faster start](https://github.blog/changelog/2026-03-19-copilot-coding-agent-now-starts-work-50-faster/)
- **Claude Code Auto-fix PRs:** Claude subscribes to a PR's GitHub activity. On a failed check or a review comment, it investigates and pushes a fix if one is clear, asks the user if the comment is ambiguous or architecturally significant, and notes duplicates. It can be turned on from the Changes card, with `/autofix-pr` from the terminal, or by telling it in chat (also from mobile).
  - Merge conflicts don't trigger it, because GitHub sends no webhook for them. You ask Claude to rebase instead.
  - Replies on GitHub are posted under the user's account but labeled as coming from Claude Code.
  - The docs warn that comment-triggered automation (Atlantis, `issue_comment` Actions) can fire as a result.

  — [Claude Code docs: cloud](https://code.claude.com/docs/en/claude-code-on-the-web)
- **Inline diff comments:** in the Claude Code cloud diff view you can comment on specific lines, and the comments go to Claude with your next message. "Compare against" lets you diff against any branch, not only the base. — [Claude Code docs: cloud](https://code.claude.com/docs/en/claude-code-on-the-web)
- **Codex code review:** once enabled on a GitHub repo, Codex reviews PRs automatically as they move from draft to ready, and on request via "@codex review". Per a third-party blog, GitHub code review is GA after DevDay 2026, and "Codex Security Cloud" scans repos. — [OpenAI: Introducing upgrades to Codex](https://openai.com/index/introducing-upgrades-to-codex/); [szymonpaluch.com (secondary)](https://szymonpaluch.com/blog/posts/openai-codex-cloud)
- **Codex desktop app:** a PR review pane, task sidebar and artifact viewer (third-party write-up, title only). — [danielvaughan (unfetchable)](https://codex.danielvaughan.com/2026/04/17/codex-app-workspace-pr-review-task-sidebar-artifact-viewer/)
- **Vibe Kanban:** each task attempt runs in its own git worktree. Built-in diffs let you inspect, edit and approve agent work like a PR, and you can preview website changes in a browser.
  - Third-party listings also describe inline feedback the agent picks up, a preview proxy with devtools and click-to-component, and dispatching 2–3 attempts to different agents and comparing their diffs. These are less certain.

  — [Vibe Kanban docs](https://www.vibekanban.com/docs/getting-started.md); [openapps listing](https://openapps.pro/apps/vibe-kanban)
- **Conductor:** one UI for diff review, checks, PR creation, merge and archiving. Run scripts start the app or tests per workspace, and `CONDUCTOR_PORT` gives each workspace its own port range so several dev servers run side by side, which enables live previews of parallel attempts. — [Conductor docs](https://www.conductor.build/docs/guides/parallel-agents/run-multiple-claude-code-sessions); [Conductor workflows](https://www.conductor.build/workflows/run-parallel-claude-codes)
- **Cursor:** Cloud Agents with "computer use" were announced in a forum thread. Agents can operate a browser/desktop, which enables UI screenshots and videos (details not verified). — [Cursor forum](https://forum.cursor.com/t/cloud-agents-with-computer-use/152829)
- **Multi-agent review:** Claude Code "Ultrareview" runs a deep multi-agent code review in a cloud sandbox. — [Claude Code docs: cloud (related resources)](https://code.claude.com/docs/en/claude-code-on-the-web)
- **Caution on review signals:** a guide warns that "a review comment or Slack request is not proof that a code change, test, or pull request completed successfully". — [search summary of Codex guides](https://blakecrosley.com/guides/codex)

### Inferences
- Minimum viable PR workflow for Queue:
  - Push the task branch, open a draft PR (GitHub/Gitea/GitLab API), and link it in the task drawer.
  - Turn PR review comments and CI failures into follow-up tasks or messages, with an "auto-fix" toggle per PR.
  - Keep Copilot's guardrails: the agent never approves or merges, and comments from non-writers are ignored.
- Queue already has diff + revert. Adding line comments on the diff that are bundled into a follow-up task is cheap and high-value.
- "Best-of-N" for a single task (run Claude, Codex and Gemini in parallel and compare diffs) fits Queue's multi-CLI design and its parallelism setting. Vibe Kanban shows the UX.
- A per-task preview (a dev server in the sandbox with a port proxy, as in Conductor's `CONDUCTOR_PORT`) plus automatic screenshots would differentiate UI tasks.

### Gaps
- No primary source confirmed for Codex "best-of-N"/multiple attempts, or for side-by-side attempt comparison in Codex or Cursor in 2026.
- No sources found for structured test-report UIs (JUnit summaries) in these products.

## 3. Integrations (GitHub/GitLab, Linear/Jira, Slack/Teams/Discord, MCP, webhooks/API/CLI, IDE)

### Takeaway
Every leader lets you start work from where it is discussed: an issue assignment or mention in GitHub, Linear or Jira, or an @mention in Slack, sometimes Teams. Results come back in the same channel, and all sessions land in one unified list. Programmatic access (API fire endpoints, a CLI for follow-ups) and MCP connectors per task or environment are standard. Queue has none of these.

### Cited Findings
- **Cursor:** on mobile go to cursor.com/agents. In Slack, GitHub or Linear, mention @Cursor. With Cursor 3 (Apr 2026), agents started from mobile, web, desktop, Slack, GitHub or Linear all appear in one place. Sources disagree on a Microsoft Teams integration (May 2026). — [Cursor help: background agents](https://cursor.com/help/ai-features/background-agents); [digitalapplied Cursor 3 guide](https://www.digitalapplied.com/blog/cursor-3-agents-window-complete-guide); [buildfastwithai](https://blog.buildfastwithai.com/cursor-cloud-agents-development-environments-2026)
- **Codex Slack:** tagging @Codex in a channel or thread makes it gather context from the conversation, pick the right environment, and reply with a link to the completed cloud task. — [OpenAI: Codex GA](https://openai.com/index/codex-now-generally-available/)
- **Codex Linear (secondary):** issues that match a rule and enter triage are auto-assigned to Codex and run under the issue creator's account. — [danielvaughan blog, search snippet](https://codex.danielvaughan.com/2026/03/27/codex-slack-linear-cloud-tasks/)
- **Linear for Agents (Developer Preview):** agent sessions are created when an agent is mentioned or assigned.
  - Assigning sets the app as *delegate*, so a human stays the owner.
  - The agent should emit a "thought" activity within 10 s.
  - Activities include reasoning steps, tool use, clarification requests and final responses.
  - Required scopes are `app:assignable` and `app:mentionable`, and a `promptContext` field summarizes issue and comments.

  — [Linear developers: agents](https://linear.app/developers/agents.md); [Linear docs: agents in Linear](https://linear.app/docs/agents-in-linear); [Linear changelog 2025-07-30 (older)](https://linear.app/changelog/2025-07-30-agent-interaction-guidelines-and-sdk)
- **GitHub Copilot coding agent:** GitHub issues and Jira (preview, Mar 2026) are entry points, and VS Code can command the agent (2025, older). — [GitHub Changelog: Jira](https://github.blog/changelog/2026-03-05-github-copilot-coding-agent-for-jira-is-now-in-public-preview/); [VS Code blog (Jul 2025)](https://code.visualstudio.com/blogs/2025/07/17/copilot-coding-agent)
- **Claude Code integrations:**
  - Claude in Slack (@Claude). Slack sessions are auto-shared with Team visibility.
  - "Claude Tag", an org-managed @Claude in Slack.
  - "Channels" to ask from Telegram, Discord or iMessage while work runs locally.
  - Connectors (MCP) are attached per scheduled task, and all connected connectors are included by default, with advice to remove unneeded ones.

  — [Claude Code docs: cloud](https://code.claude.com/docs/en/claude-code-on-the-web); [Claude Code docs: mobile](https://code.claude.com/docs/en/mobile); [Claude Code docs: scheduled tasks](https://code.claude.com/docs/en/routines)
- **Claude Code GitHub integration:** a GitHub App, or a `gh` token via `/web-setup`. Credentials stay outside the VM, and a GitHub proxy attaches them server-side. GitLab and Bitbucket can be sent as a bundle but can't be pushed back, so PRs require GitHub or GHES. — [Claude Code docs: cloud](https://code.claude.com/docs/en/claude-code-on-the-web)
- **Webhook/API ingress:** Claude scheduled tasks expose `POST …/routines/<id>/fire` with a bearer token, which is the documented way to wire in alerting, deploy pipelines and internal tools. Cursor Automations accept webhook, Sentry and PagerDuty triggers. — [Claude Code docs: scheduled tasks](https://code.claude.com/docs/en/routines); [developertoolkit.ai](https://developertoolkit.ai/en/cursor-ide/advanced-techniques/cloud-agents-automations/)
- **Jules:** has a CLI ("jules" command) and a REST API (v1alpha), and community MCP servers wrap it. — [rywalker.com](https://rywalker.com/research/jules); [Jules MCP server (community)](https://glama.ai/mcp/servers/@savethepolarbears/jules-mcp-server/tree/19d7203e616f62be11f6473d579f5eeaf057bed9)
- **Session sharing:** a session link can be made Private/Team, or Private/Public on consumer plans. Repo-access verification for recipients is on by default for Team/Enterprise, and the docs warn that sessions may contain credentials. — [Claude Code docs: cloud](https://code.claude.com/docs/en/claude-code-on-the-web)

### Inferences
- Suggested integration order for a self-hosted product:
  1. A REST API, outbound webhooks (task started/finished/needs-input) and a small CLI (`queue add`, `queue msg <id>`). Everything else builds on these.
  2. Git forge integration (GitHub App and/or Gitea/Forgejo/GitLab tokens): issue label or assignment → task, PR creation, PR comments → follow-ups, checks status.
  3. A Slack/Discord/Matrix bot: create a task from a thread, reply with a result link.
  4. Linear/Jira via Linear's Agent API, using the delegate model and activity streaming.
- Queue's existing ntfy support can be generalized into "outbound notification channels": ntfy, Slack webhook, Discord webhook, Matrix, email.
- Per-project MCP server configuration that is visible in the UI is expected. Default to least-privilege, opposite to Claude's "all connectors by default".

### Gaps
- Bitbucket/GitLab-native app support among the leaders was not found, except that Claude Code says GitLab/Bitbucket can't push back.
- IDE extensions for remote queues: only the Copilot/VS Code (2025) evidence was found.
- Devin's Slack/Linear integration details were not returned by search.

## 4. Mobile & notifications

### Takeaway
Mobile is a full client for background agents. You can start tasks, monitor them, answer questions, approve commands, review diffs, ask for a PR to be watched, and attach photos. Push notifications go out on "finished" and "needs input", and tapping one deep-links into the task. Live Activities exist in the ChatGPT app, but Live-Activity-style approvals were only confirmed in a third-party app. Queue's PWA with Web Push covers the basics, but it lacks notification granularity settings and lock-screen-style progress.

### Cited Findings
- **Claude app (iOS/Android) Code tab:** start cloud sessions by picking repo, branch and task. Sessions persist across devices. You can check progress, answer questions and steer, tell Claude to watch a PR and auto-fix it, and drive a local machine via Remote Control. Permission modes on mobile are Accept edits, Plan and Auto, without Bypass. — [Claude Code docs: mobile](https://code.claude.com/docs/en/mobile)
- **Claude push notifications:** with Remote Control, Claude sends a push "typically when a long-running task finishes or when it needs a decision from you". Users can also ask in the prompt ("notify me when the tests finish"). There are two `/config` toggles. Dispatch notifies when a spawned session finishes or needs approval. — [Claude Code docs: mobile](https://code.claude.com/docs/en/mobile)
- **Claude onboarding to mobile:** `/mobile` shows a QR code to install the app. — [Claude Code docs: mobile](https://code.claude.com/docs/en/mobile)
- **Codex on iPhone/iPad (May 2026):** push alerts when a task on the Mac app finishes or needs input, completed Codex tasks open straight from iOS notifications, and diffs can be reviewed and commands approved from the phone while work stays on the host machine. — [9to5Mac, 2026-05-21](https://9to5mac.com/2026/05/21/openai-improves-codex-ios-experience-with-turn-completion-alerts-new-commands-more/); [ChatGPT & Codex changelog](https://learn.chatgpt.com/docs/changelog); [OpenAI: Work with Codex from anywhere](https://openai.com/index/work-with-codex-from-anywhere/)
- **Live Activities:** the ChatGPT changelog notes Live Activities on iOS were re-enabled in June 2025 after a missed-notification issue (older). Live Activity and Dynamic Island approve buttons for Codex were seen only in the third-party app Moshi, not confirmed for the official app. — [ChatGPT & Codex changelog](https://learn.chatgpt.com/docs/changelog); [Moshi](https://getmoshi.app/guides/codex)
- **Practical limits:** a third-party guide says push delivery for approval prompts was inconsistent in preview. Another advises reviewing large diffs at a desk and using phone approval for summary-level decisions. — [findskill.ai](https://findskill.ai/blog/codex-mobile-chatgpt-setup-workflows/); [Verdent](https://www.verdent.ai/guides/codex-mobile-chatgpt-developers)
- **Cursor:** a mobile web at cursor.com/agents, plus an iOS app as an entry point (secondary). — [Cursor help](https://cursor.com/help/ai-features/background-agents); [digitalapplied](https://www.digitalapplied.com/blog/cursor-3-agents-window-complete-guide)

### Inferences
- For an iOS PWA, Live Activities are not available, since they need a native app. The closest alternatives are deep-linked Web Push, badges (the Badging API) and ntfy priority levels.
- Expected notification settings:
  - per-event toggles: finished, needs-input, failed, review ready, budget threshold
  - per project
  - quiet hours, with needs-input exempt
  - a daily digest of overnight runs (especially with night scheduling)
  - "notify me" requests inside a prompt
- Mobile-first review: summary-first cards with approve, request-changes or "send follow-up" actions, and the full diff behind a tap.

### Gaps
- No primary source found on notification digests, quiet hours or per-event granularity in any leading product. These are best-practice inferences, not observed features.
- Devin and Jules mobile capabilities were not found.

## 5. Dashboards & analytics

### Takeaway
Cost and usage visibility per session, per user and per org is standard at Devin. Even consumer products surface usage against plan limits. "Time saved" and success-rate dashboards were not confirmed in primary sources. Queue has budgets but no analytics.

### Cited Findings
- **Devin Session Insights:**
  - Every user sees a completed session's ACU cost.
  - Sessions are sized XS–XL by ACU and user-message thresholds.
  - A "Knowledge Usage" tab shows which knowledge items helped or misled.
  - Diagnostics read the patterns: high ACU with few messages means poor scoping, and the reverse means drip-feeding.

  — [Devin University: reading session insights](https://learndevin.com/course/cloud-103/lesson/r-reading-session-insights); [Devin docs: usage](https://docs.devin.ai/admin/billing/usage)
- **Devin org analytics:** enterprise and org admins have Consumption Analytics, and opt-in personal analytics ("My analytics") need an admin permission. A v3 API exposes session counts, ACU consumption and active users for custom dashboards, using a service user with scoped permissions. — [Devin docs: personal analytics](https://docs.devin.ai/enterprise/security-access/personal-analytics); [Devin docs: build usage dashboard](https://docs.devin.ai/use-cases/gallery/build-usage-dashboard); [Devin API: user usage analysis (beta)](https://cognitionai.mintlify.app/enterprise-api/user-usage/get-user-usage-analysis-beta)
- **Claude Code usage limits:** cloud sessions share rate limits with all other usage, so parallel tasks consume proportionally more. Scheduled tasks have explicit hourly caps. — [Claude Code docs: cloud](https://code.claude.com/docs/en/claude-code-on-the-web); [Claude Code docs: scheduled tasks](https://code.claude.com/docs/en/routines)
- **Outcome labels:** Claude scheduled task history filters on Succeeded/Failed/Cancelled, but the docs caution that this is not task success. — [Claude Code docs: scheduled tasks](https://code.claude.com/docs/en/routines)

### Inferences
- A useful Queue dashboard would show:
  - cost/tokens and wall time per task, project, user and provider
  - success rate, split into session-completed and review-passed/merged or reverted
  - re-run and revert rate
  - queue wait time
  - quota consumption with a forecast of when a provider's window resets and how many queued tasks fit
- A per-task "insights" panel in the drawer, in the style of Devin (cost, size class, number of follow-ups needed), is cheap and helps users write better prompts.

### Gaps
- No primary source found for "time saved" metrics or quota forecasting in any leading product.

## 6. Onboarding & DX (first run, auto-setup, environments, caching, docs, shortcuts, a11y, i18n)

### Takeaway
Leaders minimize first-run friction. Onboarding creates a default environment, connects git, and walks through a first task. Per-repo "environments" (setup script, env vars, network policy, secrets kept outside the sandbox) with cached setup are the core DX primitive. Queue has project settings and provider credentials but no per-project environment definition, setup caching or guided first run.

### Cited Findings
- **First-run onboarding:** it sets up a "Default" environment with Trusted network access, either creating it automatically or asking the user to, and a quickstart connects GitHub and walks through a first task. An org-level "Quick setup" toggle removes steps for members. — [Claude Code docs: cloud](https://code.claude.com/docs/en/claude-code-on-the-web)
- **Environments:** saved config for network access, environment variables and setup scripts. A setup script is cached only if it finishes in about 5 minutes.
  - A secondary source says the cache has about a 7-day TTL with the script re-run on a miss, network levels are None/Trusted/Full/Custom, and tools like gh are not preinstalled.

  — [Claude Code docs: cloud](https://code.claude.com/docs/en/claude-code-on-the-web); [ascend-docs research note (secondary, 2026-04-24)](https://ascend-docs-site.pages.dev/architecture/research/claude-code-cloud-architecture-2026-04-24/)
- **Network secrets:** keys added to an environment stay outside the sandbox and are attached to matching outbound requests by a proxy. Git credentials and signing keys also never enter the VM. — [Claude Code docs: cloud](https://code.claude.com/docs/en/claude-code-on-the-web)
- **Conductor workspace setup:** setup scripts install dependencies, generate files, create symlinks and initialize local DBs. A separate "Files to copy" list covers gitignored files like `.env.local`. Run scripts and per-workspace ports are also defined. — [Conductor docs](https://www.conductor.build/docs/guides/parallel-agents/run-multiple-claude-code-sessions); [Conductor: first workspace](https://conductor.build/docs/first-workspace)
- **Repo-level agent config:** Claude cloud sessions load `.claude/agents/` subagents and `.claude/settings.json` from the repo. Codex guides cover AGENTS.md, sandbox and MCP config. — [Claude Code docs: cloud](https://code.claude.com/docs/en/claude-code-on-the-web); [blakecrosley Codex guide](https://blakecrosley.com/guides/codex)
- **Error UX:** the docs show the pattern of returning the prompt to the input box when a session fails to start, and named, actionable error messages for every failure mode (branch not on GitHub, upload refusals). — [Claude Code docs: cloud](https://code.claude.com/docs/en/claude-code-on-the-web)
- **Sensitive-file hygiene:** when uploading a local repo, Claude Code leaves out uncommitted changes to `.env`, `*.tfvars`, `id_rsa` and `*.pem`, and lists them ("Left on this machine"). — [Claude Code docs: cloud](https://code.claude.com/docs/en/claude-code-on-the-web)
- **Lifecycle:** sessions can be archived, unarchived, deleted with confirmation, and shared. Idle environments expire, and reopening re-provisions with the conversation restored. — [Claude Code docs: cloud](https://code.claude.com/docs/en/claude-code-on-the-web)

### Inferences
- For Queue:
  - A first-run wizard: admin creates org, adds provider keys, creates or imports a first project, runs a sample task, enables push.
  - Per-project environment settings: setup script, env vars, secrets, network allowlist, cached container image or layer after setup.
  - Auto-detection that proposes build/test commands and drafts AGENTS.md/CLAUDE.md from repo files, as a "setup task" the agent itself runs.
  - Secret redaction in event logs.
- Archive versus delete, and "share task link" read-only, are small but expected.

### Gaps
- No primary source on keyboard shortcuts, WCAG/accessibility conformance or i18n of these products. Repo auto-setup detection that generates AGENTS.md is only implied, not confirmed as a product feature.

## 7. Operations for self-hosting (backups, upgrades, health, metrics, logs, runner scaling, admin, licensing)

### Takeaway
The most relevant reference architecture is Claude Code's self-hosted environments. Runners on your hosts poll a queue, hold a lease (with heartbeat), and isolate by owner. They have capacity, graceful drain, retire-at and idle-release flags, plus an autoscaling orchestrator, metrics and a health endpoint. Self-hosted tools like n8n and Gitea show the ops baseline: opt-in Prometheus `/metrics`, readiness endpoints, documented backup sets (DB + config + data dir), automatic DB migrations on start and "backup before upgrade". Queue has none of these yet.

### Cited Findings
- **Claude Code self-hosted runners: model and leases.** An environment is a named group of runners on your hosts that polls `api.anthropic.com` outbound, with no inbound connections. A runner claims a session, holds a lease, clones the repo and spawns a child agent process. Each poll refreshes the lease and acts as the heartbeat, and if polling stops, the lease lapses after about 60 s and the session is requeued. — [Claude Code docs: self-hosted environments](https://code.claude.com/docs/en/self-hosted-environments)
- **Claude Code self-hosted runners: owner isolation and scaling.**
  - A runner locks to the first session's owner and serves only that owner, up to `--capacity`, so checked-out code never mixes between users. Minimum fleet size equals the number of concurrently active owners.
  - `--drain-grace-sec` sets exit behavior. The default of 0 exits after sessions finish so the orchestrator restarts it with a fresh disk.
  - `--retire-at` covers known host kills such as spot instances, and idle sessions can be released (`--release-idle-session-min`) to resume on another runner.
  - An optional autoscaling orchestrator starts runners as sessions queue.
  - Kubernetes and Compose recipes, a CI smoke test for runner images and a reference listing every metric and the health endpoint are documented.

  — [Claude Code docs: self-hosted environments](https://code.claude.com/docs/en/self-hosted-environments)
- **Claude Code self-hosted runners: robustness.** Polls time out after 10 s with exponential backoff up to 20 s. Corporate proxies and mTLS are supported. Event streaming uses SSE, so proxies must not buffer. — [Claude Code docs: self-hosted environments](https://code.claude.com/docs/en/self-hosted-environments)
- **n8n queue mode:**
  - Workers expose `/healthz/readiness`, reporting DB and Redis readiness, when `QUEUE_HEALTH_CHECK_ACTIVE` is set.
  - Prometheus `/metrics` is off by default and enabled with `N8N_METRICS=true`, self-hosted only.
  - Queue gauges (for example `n8n_scaling_mode_queue_jobs_waiting`) come from the main process with `N8N_METRICS_INCLUDE_QUEUE_METRICS=true`.
  - A community Grafana dashboard exists for queue mode.

  — [n8n docs: enable queue mode](https://docs.n8n.io/deploy/host-n8n/configure-n8n/scaling/enable-queue-mode); [Grafana dashboard 25836](https://grafana.com/grafana/dashboards/25836-n8n-production-overview-queue-mode/); [Lumadock guide](https://lumadock.com/blog/tutorials/n8n-monitoring-prometheus-grafana-vps/)
- **Gitea upgrade:** stop, back up, start the new version. Gitea checks for and runs DB migrations on every start. The backup covers DB, config file and data dir (`APP_DATA_PATH`) plus external object storage. A Forgejo admin guide (older) warns `gitea dump` zips don't work across versions, and suggests stepping through intermediate minors for multi-major jumps. — [Gitea docs: upgrade from old Gitea](https://docs.gitea.com/upgrade/upgrade-from-gitea); [Forgejo admin guide (dated)](https://forgejo.gna.org/dachary/admin-guide)
- **Admin and policy in leaders:**
  - Org Owners toggle features (cloud sessions, `allow_remote_sessions`, Quick setup, self-hosted environments) and custom roles gate permissions.
  - Devin has enterprise and org admin consumption analytics and permissioned personal analytics.
  - Copilot ignores comments from users without write access.

  — [Claude Code docs: cloud](https://code.claude.com/docs/en/claude-code-on-the-web); [Claude Code docs: self-hosted](https://code.claude.com/docs/en/self-hosted-environments); [Devin docs](https://docs.devin.ai/enterprise/security-access/personal-analytics); [GitHub Docs](https://docs.github.com/ko/enterprise-cloud@latest/copilot/concepts/coding-agent/about-copilot-coding-agent)
- **Interrupted work:** when an idle cloud environment expires and is re-provisioned, the conversation is restored, but running shell commands, subagents and session-scoped schedules are not. Claude is told which ones stopped. — [Claude Code docs: cloud](https://code.claude.com/docs/en/claude-code-on-the-web)

### Inferences
- An ops baseline for Queue, by analogy with n8n and Gitea:
  1. A documented backup set: `pg_dump` plus workspace volumes plus config/secrets, scheduled with retention (and restic/borg offsite), with a tested restore procedure and a "backup before upgrade" hook in the update script.
  2. Automatic, versioned DB migrations on start, with a pinned-version upgrade path and release notes.
  3. `/healthz` (liveness) and `/readyz` (DB, Podman socket, provider reachability) for systemd watchdog or uptime monitors.
  4. Opt-in Prometheus `/metrics`: queue depth per user, running tasks, task duration, failure count by provider, token/cost counters, SSE connections. Optionally OpenTelemetry traces per task.
  5. Structured JSON logs to journald, with retention for workspace and event logs.
- For horizontal scaling, copy the lease/heartbeat runner model: a runner agent on extra VPS hosts polls Postgres or an API for work (for example `SELECT … FOR UPDATE SKIP LOCKED`), heartbeats via its lease, and requeues on lapse. It should keep per-user lock and capacity semantics, which matches Queue's per-user sequential queue.
- An admin console should cover users and invitations, roles, per-user quotas and budgets, provider-key management, a global queue view (pause/drain a runner) and an audit log.
- Licensing: self-hosted peers often publish under source-available or open licenses (n8n uses a "fair-code" license, which is not verified in this research). The choice should be made deliberately.

### Gaps
- Specific metric names and the health endpoint of Claude Code self-hosted runners are in the reference page, which was not fetched.
- No current primary source on Plausible's backup/upgrade docs, n8n's backup procedure, or licensing comparisons. These were not retrieved within the tool budget.
- Official Codex, Jules and Cursor docs were unreachable from the sandbox (DNS blocked for developers.openai.com, jules.google, infoq.com, codex.danielvaughan.com). A follow-up with those hosts allowed would strengthen sections 1–4.
