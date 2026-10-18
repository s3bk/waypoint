#!/usr/bin/env bash
# Build and deploy to the production host. Usage: scripts/deploy.sh [--rollback]
# Host and layout are described in DEPLOY.local.md (not in git).
set -euo pipefail
cd "$(dirname "$0")/.."
HOST=${WAYPOINT_HOST:-sebk@waypoint.qdat.net}
URL=${WAYPOINT_PUBLIC_URL:-https://waypoint.qdat.net}

if [ "${1:-}" = "--rollback" ]; then
  ssh "$HOST" 'cd ~/waypoint && mv waypoint-server.prev waypoint-server && systemctl --user restart waypoint'
else
  cargo build --profile deploy -p waypoint-server
  (cd client && npm run build)
  need=$(objdump -T target/deploy/waypoint-server | grep -o 'GLIBC_[0-9.]*' | sort -uV | tail -1)
  echo "binary needs $need; host has: $(ssh "$HOST" 'ldd --version | head -1')"
  rsync -a --delete client/dist/ "$HOST":waypoint/dist/
  scp -q target/deploy/waypoint-server "$HOST":waypoint/waypoint-server.new
  ssh "$HOST" 'cd ~/waypoint && cp waypoint-server waypoint-server.prev && mv waypoint-server.new waypoint-server && systemctl --user restart waypoint'
fi
sleep 2
curl -fsS "$URL/health" && echo " ok"
