#!/usr/bin/env bash
# Run cache acceptance gate on local audit JSONL
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
AUDIT_DIR="${ROOT}/.local-dev/audit"
DATE="${1:-$(date +%Y-%m-%d)}"
AUDIT="${AUDIT_DIR}/requests-${DATE}.jsonl"
MIN_CACHE="${MIN_CACHE:-85}"

if [[ ! -f "$AUDIT" ]]; then
  echo "FAIL: missing audit $AUDIT"
  exit 2
fi

echo "=== cache gate: $AUDIT ==="
python3 "${ROOT}/ops/cache_quality_acceptance.py" "$AUDIT" --min-cache-pct "$MIN_CACHE" "$@"

echo
echo "=== per-model R2 (python) ==="
python3 - <<PY
import json
from collections import defaultdict

path = "$AUDIT"
by = defaultdict(list)
for line in open(path):
    try:
        r = json.loads(line)
    except Exception:
        continue
    m = r.get("public_model") or r.get("model") or "?"
    if m in ("deepseek-v4-flash", "mimo-v2.5", "big-pickle"):
        by[m].append(r)

for m in sorted(by):
    rows = by[m]
    read = miss = 0
    ok = fail = 0
    raw_match = 0
    for r in rows:
        u = r.get("usage") or r
        cr = int(u.get("cache_read_input_tokens") or 0)
        cm = int(u.get("cache_miss_input_tokens") or 0)
        read += cr
        miss += cm if cm > 0 else max(int(u.get("prompt_tokens") or 0) - cr, 0)
        if r.get("outcome") == "success":
            ok += 1
        else:
            fail += 1
        cf = r.get("cache_forensics") or {}
        if cf.get("ccp_raw_prefix_match_32k"):
            raw_match += 1
    r2 = read / (read + miss) * 100 if read + miss else 0
    print(f"{m}: n={len(rows)} ok={ok} fail={fail} R2={r2:.2f}% raw_prefix_match={raw_match}/{len(rows)}")
PY
