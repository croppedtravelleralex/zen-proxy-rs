#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
TOKEN_FILE="${1:-}"

if [[ -z "$TOKEN_FILE" || ! -f "$ROOT/$TOKEN_FILE" ]]; then
  echo "usage: $0 .local-dev/.ccs-token.tmp" >&2
  exit 2
fi

TOKEN="$(cat "$ROOT/$TOKEN_FILE")"
rm -f "$ROOT/$TOKEN_FILE"

if [[ -z "$TOKEN" ]]; then
  echo "empty token file" >&2
  exit 2
fi

cd "$ROOT"
if [[ -f .local-dev/zen-proxy.pid ]]; then
  kill "$(cat .local-dev/zen-proxy.pid)" 2>/dev/null || true
fi
pkill -x zen-proxy-rs 2>/dev/null || true

nohup env PROXY_API_KEY="$TOKEN" bash ops/local-dev/run-local-zenproxy.sh \
  > .local-dev/zen-proxy.log 2>&1 &
echo $! > .local-dev/zen-proxy.pid

for _ in $(seq 1 30); do
  if curl -sf http://127.0.0.1:14000/health >/dev/null; then
    exit 0
  fi
  sleep 1
done

tail -n 80 .local-dev/zen-proxy.log >&2 || true
exit 3
