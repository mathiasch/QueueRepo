#!/usr/bin/env bash
# One-time host setup (Debian/Ubuntu). Needs root ONCE for packages, the
# service user and systemd units – nothing runs as root afterwards.
#   sudo ./deploy/install.sh
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
[[ $EUID -eq 0 ]] || { echo "run with sudo (one-time setup)"; exit 1; }

apt-get update
apt-get install -y --no-install-recommends podman uidmap slirp4netns fuse-overlayfs git ca-certificates curl build-essential pkg-config podman-compose

id queued >/dev/null 2>&1 || useradd --system --create-home --home-dir /home/queued --shell /usr/sbin/nologin queued
# Subordinate ids for rootless containers.
grep -q '^queued:' /etc/subuid || usermod --add-subuids 200000-265535 --add-subgids 200000-265535 queued
loginctl enable-linger queued

install -d -o queued -g queued /var/lib/queued/app /var/lib/queued/runner /etc/queued /opt/queued/web
install -d /opt/queued/bin
install -d /opt/queued/deploy && cp "$ROOT/deploy/compose.yml" /opt/queued/deploy/

# Build as the service user from a private copy (no root-owned artifacts).
SRC=/opt/queued/src
rm -rf "$SRC" && mkdir -p "$SRC"
tar -C "$ROOT" --exclude=./backend/target --exclude='./*/node_modules' --exclude=./.git -cf - . | tar -C "$SRC" -xf -
chown -R queued:queued "$SRC" /opt/queued/web
command -v node >/dev/null || apt-get install -y --no-install-recommends nodejs npm
sudo -u queued -H bash -lc "command -v cargo >/dev/null || curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal"
sudo -u queued -H bash -lc "cd $SRC/backend && ~/.cargo/bin/cargo build --release"
sudo -u queued -H bash -lc "cd $SRC/frontend && npm ci --no-audit --no-fund && npm run build && cp -r dist/. /opt/queued/web/"
install -m 755 "$SRC/backend/target/release/queued" "$SRC/backend/target/release/queued-runner" /opt/queued/bin/

# Agent sandbox image in the queued user's rootless storage.
sudo -u queued -H bash -lc "cd $SRC && podman build -t localhost/queued-agent:latest -f docker/agent.Dockerfile ."

for f in queued runner; do
  [[ -f /etc/queued/$f.env ]] || install -m 600 -o queued -g queued "$ROOT/deploy/$f.env.example" /etc/queued/$f.env
done
install -m 644 "$ROOT/deploy/queued.service" "$ROOT/deploy/queued-runner.service" /etc/systemd/system/
systemctl daemon-reload
echo
echo "Next: edit /etc/queued/queued.env + runner.env (same RUNNER_TOKEN: openssl rand -hex 32),"
echo "      start Postgres:  sudo -iu queued POSTGRES_PASSWORD=… podman-compose -f /opt/queued/deploy/compose.yml up -d"
echo "      then:            sudo systemctl enable --now queued-runner queued"
