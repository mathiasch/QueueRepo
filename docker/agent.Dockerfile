# Sandbox image for agent runs: one throwaway container per run.
# Contains the official agent CLIs + common build tooling. Extend as needed
# (e.g. add Rust, Go, Java) – the verify command runs in this image too.
FROM node:22-bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
      git ca-certificates curl wget ripgrep jq unzip zip \
      python3 python3-pip python3-venv build-essential \
    && rm -rf /var/lib/apt/lists/*

# Pinned CLI versions: the stream parsers and their contract tests
# (backend/crates/app/tests/fixtures/cli/<cli>-<version>/) target exactly these.
# Bump together with `agent::CLI_VERSIONS` and new fixtures.
ARG CLAUDE_CODE_VERSION=2.1.319
ARG CODEX_VERSION=0.167.0
ARG GEMINI_CLI_VERSION=0.67.0
# opencode (MIT): multi-provider harness for Mistral, Gemini API, Infomaniak,
# OpenAI-compatible endpoints, OpenRouter and LiteLLM (WP13).
ARG OPENCODE_VERSION=1.19.5
RUN npm install -g \
      @anthropic-ai/claude-code@${CLAUDE_CODE_VERSION} \
      @openai/codex@${CODEX_VERSION} \
      @google/gemini-cli@${GEMINI_CLI_VERSION} \
      opencode-ai@${OPENCODE_VERSION} \
    && npm cache clean --force \
    && claude --version | grep -F "${CLAUDE_CODE_VERSION}" \
    && codex --version | grep -F "${CODEX_VERSION}" \
    && gemini --version | grep -Fx "${GEMINI_CLI_VERSION}" \
    && opencode --version | grep -F "${OPENCODE_VERSION}"

RUN git config --system user.name "agent" \
 && git config --system user.email "agent@localhost" \
 && git config --system --add safe.directory '*'

ENV DISABLE_AUTOUPDATER=1 OPENCODE_DISABLE_AUTOUPDATE=1 CI=1
WORKDIR /workspace
