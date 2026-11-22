# Auftrag 03: Security governance compliance

- Gestartet: 2026-11-06 20:06 UTC
- Agent-Typ: `general-purpose`, Hintergrund: False

## Prompt (wörtlich)

````text
Research security, governance, compliance and legal requirements for a multi-tenant platform that runs autonomous coding agents, as of late 2026.

Context (the product being evaluated): Self-hosted web app (Rust/axum + Postgres) with a separate unprivileged "runner" service that owns project git workspaces and starts agent CLIs (Claude Code, Codex CLI, Gemini CLI) in rootless Podman containers (--cap-drop ALL, no-new-privileges, memory/cpu/pids limits). Agents have internet access (needed for web research). Users store their own provider credentials (Claude setup-token, Codex auth.json, Gemini OAuth creds, or API keys) and git tokens, encrypted with AES-256-GCM using a single MASTER_KEY. Auth: local accounts (Argon2) plus OIDC (Google, Apple, GitHub, generic). Multiple users can share projects (owner/editor/viewer). Hosted e.g. on a Hetzner VPS behind Cloudflare Tunnel; users are in Switzerland/EU.

Objective: Identify what's missing for a top-tier, trustworthy product: concrete threats, controls and obligations, with sources.

Key questions:
- Threat model for coding agents: prompt injection via repo content, issues, web pages and dependencies; data exfiltration; malicious packages/supply chain; agents leaking secrets into commits/logs; cross-tenant escape. What mitigations do leading products use (egress allowlists/proxies, network policies per task, secret scanning, gVisor/Firecracker/Kata microVMs, read-only mounts, short-lived credentials, tool permission policies)?
- Is rootless Podman with cap-drop sufficient for multi-tenant isolation, or are microVMs (Firecracker, Kata, gVisor) expected? What do OpenAI Codex cloud, GitHub Copilot coding agent, Devin, E2B, Daytona, Modal use?
- Secrets management: envelope encryption, key rotation, KMS/Vault, per-tenant keys; handling OAuth tokens of subscriptions.
- Terms of service: may users run Claude Pro/Max (via `claude setup-token`), ChatGPT Plus/Pro (Codex CLI login) and Gemini subscriptions on a self-hosted server / in automation / shared multi-user setups? Cite current Anthropic, OpenAI and Google consumer and usage policies as of 2026. Note any restrictions on automated or third-party-app usage of subscription credentials.
- Enterprise/governance features expected: audit logs, SSO/SAML/SCIM, MFA/passkeys, RBAC, admin console, data retention policies, approval policies for agent actions, policy-as-code.
- Compliance for CH/EU: Swiss revDSG/FADP, GDPR (data processing agreements with LLM providers, data transfers to US), EU AI Act relevance (transparency, logging), NIS2 relevance; what a self-hosted operator must document.
- Web app security baseline: CSRF, CSP, rate limiting/brute-force protection, session management, account recovery, security headers, dependency scanning, SBOM.

Suggested sources:
- OWASP Top 10 for LLM Applications 2025 / OWASP Agentic AI threats, NIST AI RMF, Simon Willison's "lethal trifecta" writing
- Official docs of OpenAI Codex (internet access settings), GitHub Copilot coding agent (firewall), Anthropic Claude Code security docs/sandboxing, E2B/Daytona/Modal sandbox docs, Firecracker/gVisor docs
- Anthropic Consumer Terms / Usage Policy, OpenAI Terms of Use / Codex docs on ChatGPT login, Google Gemini CLI terms
- EDÖB/FDPIC guidance, GDPR texts, EU AI Act official text

Constraints:
- Current state as of late 2026; Swiss/EU jurisdiction focus for legal topics. Clearly mark legal points as non-binding and cite exact policy passages where possible.

Save your output notes to /home/user/QueueRepo/research_notes/Funktionsumfang agentischer Coding Queues/security_governance_legal.md

**As a first step, you must read <deep-research-skill-dir>/references/researcher.md for instructions on how to conduct research.**
````
