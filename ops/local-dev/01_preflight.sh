#!/usr/bin/env bash
# Local dev preflight: Webshare whitelist + nodes + upstream reachability
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
DATA_DIR="${ROOT}/.local-dev"
NODES="${DATA_DIR}/nodes-prod.json"
mkdir -p "$DATA_DIR"

echo "=== public IP ==="
curl -sS --max-time 10 ifconfig.me || true
echo

echo "=== copy nodes from panda ==="
scp -o BatchMode=yes panda:/etc/zen-proxy-rs/nodes-prod.json "$NODES"
python3 -c "import json; n=json.load(open('$NODES')); print('nodes', len(n))"

echo "=== webshare -> opencode test (first node) ==="
NODE="$(python3 -c "import json; print(json.load(open('$NODES'))[0])")"
echo "node_prefix=${NODE:0:35}..."
HTTP="$(curl -sS -m 25 -o "${DATA_DIR}/ws-opencode.json" -w '%{http_code}' -x "$NODE" \
  'https://opencode.ai/zen/v1/models' -H 'Authorization: Bearer public' || echo fail)"
echo "http_code=${HTTP}"
head -c 180 "${DATA_DIR}/ws-opencode.json" 2>/dev/null || true
echo

echo "=== build check ==="
ZP="${ROOT}/repos/zen-proxy-rs/target/release/zen-proxy-rs"
if [[ -x "$ZP" ]]; then
  echo "zen-proxy-rs release: OK ($ZP)"
  sha256sum "$ZP"
else
  echo "zen-proxy-rs release: MISSING (will build)"
fi

echo "=== redis ==="
if redis-cli ping >/dev/null 2>&1; then
  echo "redis: PONG"
else
  echo "redis: not running (session pin will use memory fallback)"
fi

echo "=== preflight done ==="
