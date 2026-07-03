#!/usr/bin/env bash
set -euo pipefail
START_S=1751522220  # 2026-07-03 13:57 CST

echo "=== NewAPI type breakdown ==="
ssh -o BatchMode=yes panda "docker exec new-api-postgres psql -U newapi -d new-api -At -c \"
SELECT type, count(*) FROM logs
WHERE created_at >= ${START_S} AND channel_id=69 AND model_name ILIKE '%deepseek%'
GROUP BY type ORDER BY 1;\""

echo "=== NewAPI other sample (ok rows) ==="
ssh -o BatchMode=yes panda "docker exec new-api-postgres psql -U newapi -d new-api -At -c \"
SELECT left(COALESCE(other::text,''), 300) FROM logs
WHERE created_at >= ${START_S} AND channel_id=69 AND model_name ILIKE '%deepseek%' AND type=2
ORDER BY created_at DESC LIMIT 2;\""

echo "=== NewAPI prompt_tokens stats ok rows ==="
ssh -o BatchMode=yes panda "docker exec new-api-postgres psql -U newapi -d new-api -At -c \"
SELECT count(*), sum(prompt_tokens), avg(prompt_tokens)::int, max(prompt_tokens)
FROM logs WHERE created_at >= ${START_S} AND channel_id=69 AND model_name ILIKE '%deepseek%' AND type=2;\""

echo "=== ccswitch proxy_request_logs ==="
DB=/root/.cc-switch/cc-switch.db
if [ -f "$DB" ]; then
  sqlite3 "$DB" "PRAGMA table_info(proxy_request_logs);"
  sqlite3 "$DB" "SELECT COUNT(*) FROM proxy_request_logs WHERE datetime(created_at) >= '2026-07-03 13:57:00' OR created_at >= 1751522220000;"
  sqlite3 "$DB" "SELECT request_model, upstream_model, status_code, COUNT(*) FROM proxy_request_logs WHERE created_at >= 1751522220000 OR datetime(created_at) >= '2026-07-03 13:57:00' GROUP BY 1,2,3 LIMIT 20;" 2>/dev/null || \
  sqlite3 "$DB" "SELECT * FROM proxy_request_logs ORDER BY rowid DESC LIMIT 3;"
fi
