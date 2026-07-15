#!/usr/bin/env bash
set -euo pipefail

instance=${1:-1}
port=${2:-4001}
model=${3:-hy3}

ssh -o BatchMode=yes panda "bash -s -- '$instance' '$port' '$model'" <<'REMOTE'
set -euo pipefail

instance=$1
port=$2
model=$3
available_mb=$(free -m | awk '/Mem:/{print $7}')
total_mb=$(free -m | awk '/Mem:/{print $2}')
available_pct=$(awk -v available="$available_mb" -v total="$total_mb" 'BEGIN{printf "%.1f", 100*available/total}')
load1=$(awk '{print $1}' /proc/loadavg)
cpus=$(nproc)
normalized_load=$(awk -v load_value="$load1" -v cpus="$cpus" 'BEGIN{printf "%.3f", load_value/cpus}')
root_disk_pct=$(df -P / | awk 'NR==2 {gsub(/%/, "", $5); print $5}')
echo "PREFLIGHT mem_available_mb=$available_mb mem_available_pct=$available_pct normalized_load=$normalized_load root_disk_pct=$root_disk_pct"
(( available_mb >= 1024 ))
awk -v pct="$available_pct" 'BEGIN{exit !(pct >= 25.0)}'
awk -v load_value="$normalized_load" 'BEGIN{exit !(load_value <= 0.70)}'
(( root_disk_pct <= 85 ))

pid=$(systemctl show -p MainPID --value "zen-proxy-rs@${instance}")
proxy_api_key=$(tr '\0' '\n' < "/proc/${pid}/environ" | sed -n 's/^PROXY_API_KEY=//p' | head -n 1)
test -n "$proxy_api_key"

body=$(mktemp)
response=$(mktemp)
cleanup() {
  rm -f -- "$body" "$response"
}
trap cleanup EXIT

cat >"$body" <<JSON
{
  "model": "$model",
  "max_tokens": 128,
  "stream": true,
  "system": [{"type":"text","text":"Use the requested tool exactly and do not answer before the tool call."}],
  "messages": [{"role":"user","content":"Use the Bash tool to run exactly: printf ZEN_CANARY_OK"}],
  "tools": [{
    "name": "Bash",
    "description": "Run a shell command",
    "input_schema": {
      "type": "object",
      "properties": {"command":{"type":"string"}},
      "required": ["command"]
    }
  }],
  "tool_choice": {"type":"tool","name":"Bash"}
}
JSON

for attempt in 1 2; do
  status=$(curl --silent --show-error --output "$response" --write-out '%{http_code}' \
    -H "x-api-key: ${proxy_api_key}" \
    -H 'anthropic-version: 2023-06-01' \
    -H 'x-fmc-client: claude-code' \
    -H 'content-type: application/json' \
    --data-binary "@${body}" \
    "http://127.0.0.1:${port}/v1/messages")
  python3 - "$attempt" "$status" "$response" <<'PY'
import json
import pathlib
import sys

attempt, status, path = sys.argv[1:]
events = []
for line in pathlib.Path(path).read_text().splitlines():
    if line.startswith("data:"):
        try:
            events.append(json.loads(line[5:].lstrip()))
        except json.JSONDecodeError:
            pass
tool_name = None
input_fragments = []
cache_read = 0
for event in events:
    block = event.get("content_block") if isinstance(event, dict) else None
    if isinstance(block, dict) and block.get("type") == "tool_use":
        tool_name = block.get("name")
    delta = event.get("delta") if isinstance(event, dict) else None
    if isinstance(delta, dict) and isinstance(delta.get("partial_json"), str):
        input_fragments.append(delta["partial_json"])
    usage = event.get("usage") if isinstance(event, dict) else None
    if isinstance(usage, dict):
        cache_read = max(cache_read, usage.get("cache_read_input_tokens", 0) or 0)
try:
    tool_input = json.loads("".join(input_fragments)) if input_fragments else {}
except json.JSONDecodeError:
    tool_input = {}
ok = (
    status == "200"
    and tool_name == "Bash"
    and tool_input.get("command") == "printf ZEN_CANARY_OK"
    and any(event.get("type") == "message_stop" for event in events if isinstance(event, dict))
)
message_stop = any(event.get("type") == "message_stop" for event in events if isinstance(event, dict))
print(
    f"CLAUDECODE_CANARY attempt={attempt} status={status} "
    f"tool_use={str(ok).lower()} tool_name={tool_name or '-'} "
    f"tool_command={tool_input.get('command', '-')!r} fragments={len(input_fragments)} "
    f"message_stop={str(message_stop).lower()} cache_read_input_tokens={cache_read}"
)
if not ok:
    print("CANARY_RESPONSE_PREFIX=" + pathlib.Path(path).read_text()[:1000].replace("\n", "\\n"))
    raise SystemExit(31)
PY
  sleep 1
done

post_available_mb=$(free -m | awk '/Mem:/{print $7}')
post_load1=$(awk '{print $1}' /proc/loadavg)
post_normalized_load=$(awk -v load_value="$post_load1" -v cpus="$cpus" 'BEGIN{printf "%.3f", load_value/cpus}')
echo "POSTCHECK mem_available_mb=$post_available_mb normalized_load=$post_normalized_load"
(( post_available_mb >= 768 ))
awk -v load_value="$post_normalized_load" 'BEGIN{exit !(load_value <= 1.0)}'
REMOTE
