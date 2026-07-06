#!/usr/bin/env bash
# Multi-round cache matrix against LOCAL zen-proxy (not production).
# Requires: local zen-proxy on PORT from local.env (default 14000)
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
DEV_DIR="${ROOT}/ops/local-dev"
# shellcheck disable=SC1090
source "${DEV_DIR}/local.env" 2>/dev/null || source "${DEV_DIR}/local.env.example"

BASE="http://${BIND_ADDRESS}:${PORT}"
ROUNDS="${ROUNDS:-3}"
MODELS="${MODELS:-deepseek-v4-flash,mimo-v2.5,big-pickle}"
RUNNER="${ROOT}/repos/free-model-client-rs/scripts/panda_pressure_runner.py"

if ! curl -sf "${BASE}/health" >/dev/null 2>&1; then
  echo "Local zen-proxy not reachable at ${BASE}; start: bash ops/local-dev/run-local-zenproxy.sh"
  exit 1
fi

if [[ -z "${LOCAL_ZEN_API_KEY:-}" && -z "${PANDA_NEWAPI_KEY:-}" && -z "${OPENAI_API_KEY:-}" ]]; then
  export OPENAI_API_KEY="${PROXY_API_KEY:-local-dev-proxy}"
fi

echo "=== cache matrix base=${BASE} rounds=${ROUNDS} models=${MODELS} ==="

for round in $(seq 1 "$ROUNDS"); do
  echo "--- round ${round}/${ROUNDS} ---"
  python3 "$RUNNER" \
    --mode smoke \
    --base-url "$BASE" \
    --allow-local-panda-base \
    --force \
    --models "$MODELS" \
    --clients wsl-claudecode \
    --timeout-ms 300000 \
    --run-dir "${ROOT}/.local-dev/runs/round-${round}-$(date +%H%M%S)" \
    || echo "round ${round} had failures (continuing)"
  sleep 5
done

echo "=== gate ==="
bash "${DEV_DIR}/cache_gate.sh"
