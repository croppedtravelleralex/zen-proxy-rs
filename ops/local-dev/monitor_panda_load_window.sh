#!/usr/bin/env bash
# Background monitor during Pi load test. Run ON panda or via: ssh panda bash -s < this.sh START END OUT
set -euo pipefail
START_EPOCH="${1:-$(date +%s)}"
END_EPOCH="${2:-$((START_EPOCH + 360))}"
OUT="${3:-/tmp/panda_load_monitor.jsonl}"
INTERVAL="${INTERVAL:-30}"

sql_health() {
  curl -fsS http://127.0.0.1:4011/health 2>/dev/null || echo '{"status":"down"}'
}

sql_ch109_window() {
  local since=$1 until=$2
  docker exec new-api-postgres psql -U newapi -d new-api -At -F'|' -c \
    "SELECT COUNT(*), COUNT(*) FILTER (WHERE type=2), COUNT(*) FILTER (WHERE type=5), \
     ROUND(AVG(use_time)), ROUND(AVG(prompt_tokens)), ROUND(AVG(completion_tokens)) \
     FROM logs WHERE channel_id=109 AND created_at>=${since} AND created_at<=${until};"
}

echo "{\"event\":\"monitor_start\",\"start\":$START_EPOCH,\"end\":$END_EPOCH,\"out\":\"$OUT\"}"

while true; do
  now=$(date +%s)
  if [[ "$now" -gt "$END_EPOCH" ]]; then
    break
  fi
  health=$(sql_health)
  ch109=$(sql_ch109_window "$START_EPOCH" "$now" 2>/dev/null || echo "err")
  line=$(jq -nc \
    --argjson ts "$now" \
    --arg health "$health" \
    --arg ch109 "$ch109" \
    '{ts:$ts, health: ($health|fromjson? // $health), ch109_window: $ch109}')
  echo "$line" >> "$OUT"
  echo "$line"
  sleep "$INTERVAL"
done

echo "{\"event\":\"monitor_end\",\"ts\":$(date +%s)}"
