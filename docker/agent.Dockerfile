# Sandbox image for agent runs: one throwaway container per run.
# Contains the official agent CLIs + common build tooling. Extend as needed
# (e.g. add Rust, Go, Java) – the verify command runs in this image too.
FROM node:22-bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
      git ca-certificates curl wget ripgrep jq unzip zip \
      python3 python3-pip python3-venv build-essential \
    && rm -rf /var/lib/apt/lists/*

ARG CLAUDE_CODE_VERSION=latest
ARG CODEX_VERSION=latest
ARG GEMINI_CLI_VERSION=latest
RUN npm install -g \
      @anthropic-ai/claude-code@${CLAUDE_CODE_VERSION} \
      @openai/codex@${CODEX_VERSION} \
      @google/gemini-cli@${GEMINI_CLI_VERSION} \
    && npm cache clean --force

RUN git config --system user.name "agent" \
 && git config --system user.email "agent@localhost" \
 && git config --system --add safe.directory '*'

ENV DISABLE_AUTOUPDATER=1 CI=1
WORKDIR /workspace
