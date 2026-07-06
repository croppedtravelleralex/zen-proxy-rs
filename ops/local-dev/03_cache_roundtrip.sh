#!/usr/bin/env bash
# Two-round cache probe via Anthropic /v1/messages (ClaudeCode main path)
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
BASE="${BASE:-http://127.0.0.1:14000}"
KEY="${OPENAI_API_KEY:-local-dev-proxy}"

python3 - <<PY
import json, os, urllib.request

base = "$BASE"
key = "$KEY"
headers = {
    "Authorization": f"Bearer {key}",
    "Content-Type": "application/json",
    "x-fmc-client": "claude-code",
    "anthropic-version": "2023-06-01",
}

def messages(body: dict) -> dict:
    data = json.dumps(body).encode()
    req = urllib.request.Request(f"{base}/v1/messages", data=data, headers=headers)
    with urllib.request.urlopen(req, timeout=120) as resp:
        return json.loads(resp.read())

prefix = "x" * 8000
r1 = messages({
    "model": "deepseek-v4-flash",
    "max_tokens": 32,
    "messages": [{"role": "user", "content": prefix + "\\nReply exactly CACHE_R1"}],
})
u1 = r1.get("usage") or {}
print("round1_usage", u1)

r2 = messages({
    "model": "deepseek-v4-flash",
    "max_tokens": 32,
    "messages": [
        {"role": "user", "content": prefix + "\\nReply exactly CACHE_R1"},
        {"role": "assistant", "content": [{"type": "text", "text": "CACHE_R1"}]},
        {"role": "user", "content": "Reply exactly CACHE_R2"},
    ],
})
u2 = r2.get("usage") or {}
print("round2_usage", u2)

cr2 = int(u2.get("cache_read_input_tokens") or 0)
miss2 = int(u2.get("cache_miss_input_tokens") or 0)
inp2 = int(u2.get("input_tokens") or 0)
if miss2 <= 0 and inp2 > cr2:
    miss2 = inp2 - cr2
den = cr2 + miss2
r2_pct = cr2 / den * 100 if den else 0
print(f"round2_R2={r2_pct:.1f}% read={cr2} miss={miss2}")
PY

echo "=== audit tail ==="
tail -2 "${ROOT}/.local-dev/audit/requests-$(date +%Y-%m-%d).jsonl" | python3 -c "
import json,sys
for line in sys.stdin:
    r=json.loads(line)
    print(r.get('protocol'), r.get('outcome'), r.get('usage',{}).get('cache_read_input_tokens'), r.get('cache_forensics',{}).get('ccp_raw_prefix_match_32k'))
"
