#!/usr/bin/env bash
# Full local acceptance: zen-proxy + cache roundtrip + WSL/Windows ClaudeCode tool matrix
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
DEV_DIR="${ROOT}/ops/local-dev"
DATA_DIR="${ROOT}/.local-dev"
RUN_ID="$(date +%Y%m%d-%H%M%S)"
LOG="${DATA_DIR}/acceptance-${RUN_ID}.log"
mkdir -p "$DATA_DIR" "$DATA_DIR/runs"

exec > >(tee -a "$LOG") 2>&1

echo "=== local full acceptance run_id=${RUN_ID} ==="

# --- env ---
if [[ ! -f "${DEV_DIR}/local.env" ]]; then
  cp "${DEV_DIR}/local.env.example" "${DEV_DIR}/local.env"
fi
# shellcheck disable=SC1090
set -a
source "${DEV_DIR}/local.env"
set +a

export OPENAI_API_KEY="${OPENAI_API_KEY:-${PROXY_API_KEY:-local-dev-proxy}}"
export ANTHROPIC_API_KEY="$OPENAI_API_KEY"
export ANTHROPIC_AUTH_TOKEN="$OPENAI_API_KEY"

BASE="http://${BIND_ADDRESS}:${PORT}"

# --- preflight ---
bash "${DEV_DIR}/01_preflight.sh"

# --- start zen-proxy if needed ---
if ! curl -sf "${BASE}/health" >/dev/null 2>&1; then
  echo "Starting zen-proxy on ${BASE}..."
  nohup bash "${DEV_DIR}/run-local-zenproxy.sh" > "${DATA_DIR}/zen-proxy.log" 2>&1 &
  ZP_PID=$!
  echo "zen-proxy pid=${ZP_PID}"
  for _ in $(seq 1 20); do
    sleep 1
    curl -sf "${BASE}/health" >/dev/null 2>&1 && break
  done
fi
curl -sf "${BASE}/health" | head -c 300
echo

# --- cache roundtrip (Anthropic) ---
echo "=== 03_cache_roundtrip ==="
bash "${DEV_DIR}/03_cache_roundtrip.sh" || echo "cache roundtrip failed (continuing)"

# --- pressure runner: WSL + Windows smoke (5 cases × 3 models rotated) ---
echo "=== pressure runner smoke (wsl-claudecode) ==="
python3 "${ROOT}/repos/free-model-client-rs/scripts/panda_pressure_runner.py" \
  --mode smoke \
  --base-url "$BASE" \
  --allow-local-panda-base \
  --force \
  --models "deepseek-v4-flash,mimo-v2.5,big-pickle" \
  --clients wsl-claudecode \
  --timeout-ms 300000 \
  --run-dir "${DATA_DIR}/runs/pressure-wsl-${RUN_ID}" \
  || echo "pressure wsl had failures"

if bash -lc "powershell.exe -NoProfile -Command \"if (Get-Command claude -ErrorAction SilentlyContinue) { exit 0 } else { exit 7 }\"" 2>/dev/null; then
  echo "=== pressure runner smoke (windows-claudecode via bridge) ==="
  python3 "${ROOT}/repos/free-model-client-rs/scripts/panda_pressure_runner.py" \
    --mode smoke \
    --base-url "$BASE" \
    --allow-local-panda-base \
    --force \
    --models "deepseek-v4-flash,mimo-v2.5,big-pickle" \
    --clients windows-claudecode \
    --timeout-ms 300000 \
    --run-dir "${DATA_DIR}/runs/pressure-win-${RUN_ID}" \
    || echo "pressure windows had failures"
else
  echo "SKIP windows-claudecode: claude not found on Windows host"
fi

# --- ClaudeCode tool matrix: bash/webfetch/websearch × stream-json × 3 models × wsl+windows ---
ACCEPT_SCRIPT="${ROOT}/repos/zen-proxy-rs/scripts/run_claudecode_acceptance.py"
echo "=== claudecode acceptance suite=smoke platform=both ==="
export ANTHROPIC_API_KEY
python3 "$ACCEPT_SCRIPT" \
  --execute \
  --base-url "$BASE" \
  --api-key-env ANTHROPIC_API_KEY \
  --platform both \
  --models deepseek-v4-flash mimo-v2.5 big-pickle \
  --suite smoke \
  --output-formats stream-json \
  --permission-mode bypassPermissions \
  --timeout 300 \
  --post-case-delay 2 \
  --allow-wsl-windows-bridge \
  --run-id "local-${RUN_ID}" \
  --out-dir "${DATA_DIR}/runs/acceptance-${RUN_ID}" \
  || echo "claudecode acceptance had failures"

# --- cache gate on local audit ---
echo "=== cache gate ==="
bash "${DEV_DIR}/cache_gate.sh" || true

echo "=== audit summary ==="
AUDIT_FILE="${AUDIT_LOG_DIR}/requests-$(date +%Y-%m-%d).jsonl"
python3 - <<PY
import json
from collections import defaultdict
from pathlib import Path

path = Path("${AUDIT_FILE}")
if not path.exists():
    print("no audit", path)
    raise SystemExit(0)
by = defaultdict(list)
for line in path.read_text().splitlines():
    try: r = json.loads(line)
    except: continue
    m = r.get("public_model") or "?"
    if m in ("deepseek-v4-flash","mimo-v2.5","big-pickle"):
        by[m].append(r)
for m in sorted(by):
    rows = by[m]
    read = miss = ok = fail = 0
    for r in rows:
        u = r.get("usage") or r
        cr = int(u.get("cache_read_input_tokens") or 0)
        cm = int(u.get("cache_miss_input_tokens") or 0)
        read += cr
        miss += cm if cm > 0 else max(int(u.get("prompt_tokens") or 0) - cr, 0)
        if r.get("outcome") == "success": ok += 1
        else: fail += 1
    r2 = read/(read+miss)*100 if read+miss else 0
    print(f"{m}: n={len(rows)} ok={ok} fail={fail} R2={r2:.2f}%")
print("audit_file", path)
PY

echo "=== done log=${LOG} ==="
