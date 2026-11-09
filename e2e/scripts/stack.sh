#!/usr/bin/env bash
# Starts the full stack for system/E2E tests: fresh database, real runner
# binary (engine=none) and real app binary (mock provider) + built frontend.
# Used as Playwright webServer; runs in the foreground until killed.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
ADMIN_URL="${E2E_DATABASE_ADMIN_URL:-postgres://queued:queued@localhost:5432/postgres}"
APP_PORT="${E2E_APP_PORT:-18080}"
RUNNER_PORT="${E2E_RUNNER_PORT:-18090}"
DB="queued_e2e_$(date +%s)_$$"
WORK="$(mktemp -d)"
TOKEN="e2e-runner-token-$(head -c 12 /dev/urandom | od -An -tx1 | tr -d ' \n')"

cleanup() {
  kill "${RUNNER_PID:-}" "${APP_PID:-}" 2>/dev/null || true
  psql "$ADMIN_URL" -qc "DROP DATABASE IF EXISTS $DB WITH (FORCE)" >/dev/null 2>&1 || true
  rm -rf "$WORK"
}
trap cleanup EXIT INT TERM

if [[ "${E2E_SKIP_BUILD:-}" != "1" ]]; then
  (cd "$ROOT/backend" && cargo build --quiet --bin queued --bin queued-runner)
  (cd "$ROOT/frontend" && trunk build --release >/dev/null)
fi
psql "$ADMIN_URL" -qc "CREATE DATABASE $DB" >/dev/null
DB_URL="${ADMIN_URL%/*}/$DB"

# 127.0.0.1 as an internal host the egress proxy may reach: system tests use
# a local fake upstream instead of real external hosts (WP02).
RUNNER_ALLOW_ROOT=1 RUNNER_ENGINE=none RUNNER_TOKEN="$TOKEN" RUNNER_DATA_DIR="$WORK/runner" \
RUNNER_EGRESS_PRIVATE_HOSTS=127.0.0.1 \
RUNNER_BIND="127.0.0.1:$RUNNER_PORT" RUNNER_KILL_GRACE_SECS=3 RUST_LOG=warn \
  "$ROOT/backend/target/debug/queued-runner" &
RUNNER_PID=$!

DATABASE_URL="$DB_URL" RUNNER_URL="http://127.0.0.1:$RUNNER_PORT" RUNNER_TOKEN="$TOKEN" \
DATA_DIR="$WORK/app" FRONTEND_DIR="$ROOT/frontend/dist" BIND="127.0.0.1:$APP_PORT" \
PUBLIC_URL="http://127.0.0.1:$APP_PORT" ENABLE_MOCK_PROVIDER=true TICK_SECS=1 RUST_LOG=warn \
ALLOW_LOCAL_GIT_REMOTES=true \
TRANSIENT_BACKOFF_BASE_SECS=1 TRANSIENT_BACKOFF_MAX_SECS=2 TRANSIENT_MAX_RETRIES=2 \
  "$ROOT/backend/target/debug/queued" &
APP_PID=$!

echo "stack: app :$APP_PORT runner :$RUNNER_PORT db $DB"
wait -n "$RUNNER_PID" "$APP_PID"
