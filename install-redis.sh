#!/usr/bin/env bash
# Managed private Redis for Eco estates. Redis is an accelerator/stream log,
# never a public endpoint; MongoDB remains the durable system of record.
set -euo pipefail
server="$(command -v redis-server || true)"
if [[ -x "$server" ]]; then
  exec "$server" --daemonize yes --port "${REDIS_PORT:-6379}" --bind 127.0.0.1 --appendonly yes --appendfsync everysec
fi
echo "redis-server is not installed; run eco provision first" >&2
exit 1
