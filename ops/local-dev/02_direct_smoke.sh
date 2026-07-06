#!/usr/bin/env bash
# Direct upstream smoke: webshare-only vs local zen-proxy
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
NODES="${ROOT}/.local-dev/nodes-prod.json"
NODE="$(python3 -c "import json; print(json.load(open('$NODES'))[0])")"
echo "node=${NODE:0:30}..."

echo "=== A) webshare direct chat non-stream ==="
curl -sS -m 90 -x "$NODE" https://opencode.ai/zen/v1/chat/completions \
  -H "Authorization: Bearer public" \
  -H "Content-Type: application/json" \
  -d '{"model":"deepseek-v4-flash","messages":[{"role":"user","content":"Reply OK"}],"stream":false,"max_tokens":16}' \
  -w "\nhttp=%{http_code}\n" | tail -6

echo "=== B) local zen-proxy non-stream ==="
curl -sS -m 90 http://127.0.0.1:14000/v1/chat/completions \
  -H "Authorization: Bearer local-dev-proxy" \
  -H "Content-Type: application/json" \
  -H "x-fmc-client: claude-code" \
  -d '{"model":"deepseek-v4-flash","messages":[{"role":"user","content":"Reply OK"}],"stream":false,"max_tokens":16}' \
  -w "\nhttp=%{http_code}\n" | tail -6

echo "=== C) local zen-proxy stream ==="
curl -sS -m 90 http://127.0.0.1:14000/v1/chat/completions \
  -H "Authorization: Bearer local-dev-proxy" \
  -H "Content-Type: application/json" \
  -H "x-fmc-client: claude-code" \
  -d '{"model":"deepseek-v4-flash","messages":[{"role":"user","content":"Reply OK"}],"stream":true,"max_tokens":16}' \
  -w "\nhttp=%{http_code}\n" | head -c 400
echo
