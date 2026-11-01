# --- frontend ---------------------------------------------------------------
FROM node:22-bookworm-slim AS web
WORKDIR /web
COPY frontend/package.json frontend/package-lock.json ./
RUN npm ci --no-audit --no-fund
COPY frontend/ ./
RUN npm run build

# --- backend ----------------------------------------------------------------
FROM rust:1-bookworm AS api
WORKDIR /app
COPY backend/Cargo.toml backend/Cargo.lock ./
RUN mkdir src && echo 'fn main() {}' > src/main.rs && cargo build --release && rm -rf src
COPY backend/ ./
RUN touch src/main.rs && cargo build --release

# --- runtime ----------------------------------------------------------------
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends git ca-certificates \
    && rm -rf /var/lib/apt/lists/*
# Docker CLI only – talks to the host daemon via the mounted socket.
COPY --from=docker:27-cli /usr/local/bin/docker /usr/local/bin/docker
COPY --from=api /app/target/release/queued /usr/local/bin/queued
COPY --from=web /web/dist /srv/web
ENV FRONTEND_DIR=/srv/web BIND=0.0.0.0:8080
EXPOSE 8080
CMD ["queued"]
