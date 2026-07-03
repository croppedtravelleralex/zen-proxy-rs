#!/usr/bin/env bash
# 部署后 smoke：大 body 请求触发 USK/affinity，并检查 audit 是否写入 CCP 字段
set -euo pipefail
PANDA="${PANDA:-panda}"
PORT="${PORT:-4000}"
MODEL="${MODEL:-deepseek-v4-flash}"
# ~40k 字符前缀，满足 affinity 门槛
PAYLOAD=$(python3 -c "import json; print(json.dumps({'model':'$MODEL','messages':[{'role':'user','content':'x'*40000}],'stream':False,'max_tokens':16}))")

echo "==> probe POST /v1/chat/completions"
HTTP=$(curl -sS -o /tmp/zen-probe-out.json -w '%{http_code}' \
  -H "Authorization: Bearer ${ZEN_API_KEY:-sk-probe}" \
  -H "x-fmc-client: claude-code" \
  -H "Content-Type: application/json" \
  -d "$PAYLOAD" \
  "http://${PANDA}:${PORT}/v1/chat/completions" 2>/dev/null || \
  ssh -o BatchMode=yes "$PANDA" "curl -sS -o /tmp/zen-probe-out.json -w '%{http_code}' \
    -H 'Authorization: Bearer ${ZEN_API_KEY:-sk-probe}' \
    -H 'x-fmc-client: claude-code' \
    -H 'Content-Type: application/json' \
    -d '$PAYLOAD' \
    http://127.0.0.1:${PORT}/v1/chat/completions")

echo "http_status=$HTTP"
head -c 200 /tmp/zen-probe-out.json 2>/dev/null || ssh "$PANDA" head -c 200 /tmp/zen-probe-out.json
echo

echo "==> latest audit line (remote)"
ssh -o BatchMode=yes "$PANDA" 'tail -1 /var/log/zen-proxy-rs/audit/requests-$(date +%Y-%m-%d).jsonl' | python3 -c "
import json,sys
line=sys.stdin.read().strip()
if not line:
    print('EMPTY_AUDIT'); sys.exit(1)
r=json.loads(line)
for k in ('usk','icp_scope','prefix_32k_hash','affinity_hit','session_pin_hit','warmup_state','prompt_cache_key','public_model'):
    print(f'{k}={r.get(k)!r}')
"
