#!/usr/bin/env bash
set -euo pipefail

BASE_URL="${BASE_URL:-http://127.0.0.1:4000}"
API_KEY="${API_KEY:-sk-dev}"
ADMIN_KEY="${ADMIN_KEY:-test-key}"
MODEL="${MODEL:-deepseek-v4-flash}"

curl_json() {
  local name="$1"
  shift
  printf '\n== %s ==\n' "$name"
  curl -sS -w '\nHTTP=%{http_code} total=%{time_total}\n' "$@"
}

curl_json "models" \
  -H "Authorization: Bearer ${API_KEY}" \
  "${BASE_URL}/v1/models"

curl_json "runtime" \
  -H "x-api-key: ${ADMIN_KEY}" \
  "${BASE_URL}/admin/runtime"

curl_json "short non-stream" \
  -H "Authorization: Bearer ${API_KEY}" \
  -H "Content-Type: application/json" \
  "${BASE_URL}/v1/chat/completions" \
  -d "{\"model\":\"${MODEL}\",\"stream\":false,\"messages\":[{\"role\":\"user\",\"content\":\"reply PASS only\"}],\"max_tokens\":16}"

curl_json "short stream" \
  -H "Authorization: Bearer ${API_KEY}" \
  -H "Content-Type: application/json" \
  "${BASE_URL}/v1/chat/completions" \
  -d "{\"model\":\"${MODEL}\",\"stream\":true,\"messages\":[{\"role\":\"user\",\"content\":\"reply PASS only\"}],\"max_tokens\":16}"

if [[ "${RUN_LARGE:-0}" == "1" ]]; then
  tmp="$(mktemp)"
  python3 - "$tmp" "${LARGE_MB:-1}" "${MODEL}" <<'PY'
import json, sys
path, mb, model = sys.argv[1], int(sys.argv[2]), sys.argv[3]
payload = "x" * (mb * 1024 * 1024)
body = {
    "model": model,
    "stream": False,
    "messages": [{"role": "user", "content": payload}],
    "max_tokens": 8,
}
open(path, "w").write(json.dumps(body))
PY
  curl_json "large body ${LARGE_MB:-1}MB" \
    -H "Authorization: Bearer ${API_KEY}" \
    -H "Content-Type: application/json" \
    "${BASE_URL}/v1/chat/completions" \
    --data-binary "@${tmp}"
  rm -f "$tmp"
fi
