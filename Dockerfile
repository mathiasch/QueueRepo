# Container image for the app only (orchestrator, unprivileged). The runner is
# deployed on the host (see deploy/) – no docker socket in here.
#
# Web frontend: Rust/Leptos compiled to WebAssembly with Trunk. wasm-bindgen-cli
# must match the `wasm-bindgen` version pinned in frontend/Cargo.toml.
FROM rust:1-bookworm AS web
RUN rustup target add wasm32-unknown-unknown \
 && cargo install --locked trunk@0.22.1 wasm-bindgen-cli@0.2.130
WORKDIR /src
# The frontend shares the API types with the backend (backend/crates/api).
COPY backend/ ./backend/
COPY frontend/ ./frontend/
RUN cd frontend && trunk build --release

FROM rust:1-bookworm AS api
WORKDIR /src
COPY backend/ ./
RUN cargo build --release --bin queued

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/* \
 && useradd --system --uid 10001 --create-home queued
COPY --from=api /src/target/release/queued /usr/local/bin/queued
COPY --from=web /src/frontend/dist /srv/web
USER 10001
ENV FRONTEND_DIR=/srv/web BIND=0.0.0.0:8080 DATA_DIR=/home/queued/data
EXPOSE 8080
CMD ["queued"]
