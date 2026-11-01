# Container image for the app only (orchestrator, unprivileged). The runner is
# deployed on the host (see deploy/) – no docker socket in here.
FROM node:22-bookworm-slim AS web
WORKDIR /web
COPY frontend/package.json frontend/package-lock.json ./
RUN npm ci --no-audit --no-fund
COPY frontend/ ./
RUN npm run build

FROM rust:1-bookworm AS api
WORKDIR /src
COPY backend/ ./
RUN cargo build --release --bin queued

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/* \
 && useradd --system --uid 10001 --create-home queued
COPY --from=api /src/target/release/queued /usr/local/bin/queued
COPY --from=web /web/dist /srv/web
USER 10001
ENV FRONTEND_DIR=/srv/web BIND=0.0.0.0:8080 DATA_DIR=/home/queued/data
EXPOSE 8080
CMD ["queued"]
