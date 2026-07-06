#!/usr/bin/env bash
set -euo pipefail

echo "=== NewAPI 24h ==="
ssh -o BatchMode=yes panda 'docker exec new-api-postgres psql -U newapi -d new-api -At -c "
SELECT model_name, count(*) total,
  sum(case when type=2 then 1 else 0 end) ok,
  sum(case when type!=2 then 1 else 0 end) err,
  sum(case when type=2 then prompt_tokens else 0 end) prompt,
  sum(case when type=2 then coalesce((other::jsonb->>'\''cache_tokens'\'')::bigint,0) else 0 end) cache_tokens
FROM logs WHERE channel_id=69
  AND created_at >= extract(epoch from now() - interval '\''24 hours'\'')::bigint
  AND model_name IN ('\''deepseek-v4-flash'\'','\''mimo-v2.5'\'','\''big-pickle'\'')
GROUP BY model_name ORDER BY model_name;"'

echo "=== NewAPI 6h ==="
ssh -o BatchMode=yes panda 'docker exec new-api-postgres psql -U newapi -d new-api -At -c "
SELECT model_name, count(*) total,
  sum(case when type=2 then 1 else 0 end) ok,
  sum(case when type!=2 then 1 else 0 end) err,
  sum(case when type=2 then prompt_tokens else 0 end) prompt,
  sum(case when type=2 then coalesce((other::jsonb->>'\''cache_tokens'\'')::bigint,0) else 0 end) cache_tokens
FROM logs WHERE channel_id=69
  AND created_at >= extract(epoch from now() - interval '\''6 hours'\'')::bigint
  AND model_name IN ('\''deepseek-v4-flash'\'','\''mimo-v2.5'\'','\''big-pickle'\'')
GROUP BY model_name ORDER BY model_name;"'

echo "=== NewAPI errors 6h ==="
ssh -o BatchMode=yes panda 'docker exec new-api-postgres psql -U newapi -d new-api -At -c "
SELECT model_name, count(*), left(content,100)
FROM logs WHERE channel_id=69 AND type!=2
  AND created_at >= extract(epoch from now() - interval '\''6 hours'\'')::bigint
  AND model_name IN ('\''deepseek-v4-flash'\'','\''mimo-v2.5'\'','\''big-pickle'\'')
GROUP BY model_name, left(content,100) ORDER BY count DESC LIMIT 15;"'

echo "=== Audit analysis today ==="
ssh -o BatchMode=yes panda 'python3 - <<'"'"'PY'"'"'
import json
from collections import defaultdict

path = "/var/log/zen-proxy-rs/audit/requests-2026-07-06.jsonl"
by = defaultdict(list)
for line in open(path):
    try:
        r = json.loads(line)
    except Exception:
        continue
    m = r.get("upstream_model") or r.get("model") or ""
    if m in ("deepseek-v4-flash", "mimo-v2.5", "big-pickle"):
        by[m].append(r)

for m in sorted(by):
    rows = by[m]
    read = miss = 0
    usk = 0
    prefixes = set()
    usks = set()
    ok = fail = 0
    steady_read = steady_miss = 0
    for r in rows:
        u = r.get("usage") or r
        cr = int(u.get("cache_read_input_tokens") or 0)
        cm = int(u.get("cache_miss_input_tokens") or 0)
        read += cr
        if cm > 0:
            miss += cm
        else:
            miss += max(int(u.get("prompt_tokens") or 0) - cr, 0)
        if r.get("usk"):
            usk += 1
        if r.get("prefix_32k_hash"):
            prefixes.add(r["prefix_32k_hash"])
        if r.get("usk"):
            usks.add(r["usk"])
        outcome = r.get("outcome") or r.get("status") or ""
        if outcome in ("success", "ok"):
            ok += 1
            steady_read += cr
            steady_miss += cm if cm > 0 else max(int(u.get("prompt_tokens") or 0) - cr, 0)
        else:
            fail += 1
    r2 = read / (read + miss) * 100 if read + miss else 0
    sr2 = steady_read / (steady_read + steady_miss) * 100 if steady_read + steady_miss else 0
    print(f"{m}: rows={len(rows)} ok={ok} fail={fail} R2_all={r2:.2f}% R2_ok={sr2:.2f}% usk_pct={usk/len(rows)*100:.1f}% unique_prefix={len(prefixes)} unique_usk={len(usks)}")

# fork reasons
print("=== fork reasons ===")
fork = defaultdict(lambda: defaultdict(int))
for m, rows in by.items():
    for r in rows:
        fr = r.get("icp_fork_reason") or r.get("cache_forensics", {}).get("fork_reason") or "none"
        fork[m][fr] += 1
for m in sorted(fork):
    top = sorted(fork[m].items(), key=lambda x: -x[1])[:5]
    print(f"{m}: {top}")

# raw prefix match
print("=== ccp_raw_prefix_match ===")
for m in sorted(by):
    match = sum(1 for r in by[m] if (r.get("cache_forensics") or {}).get("ccp_raw_prefix_match_32k"))
    print(f"{m}: match={match}/{len(by[m])}")
PY'
