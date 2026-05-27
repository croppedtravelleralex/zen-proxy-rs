#!/usr/bin/env bash
set -euo pipefail

MODE="${1:-plan}"
if [[ $# -gt 0 ]]; then
  shift
fi

NEWAPI_BASE_URL="${NEWAPI_BASE_URL:-http://127.0.0.1:8081}"
ZEN_BASE_URL="${ZEN_BASE_URL:-http://127.0.0.1:4000}"
ZEN_ADMIN_KEY="${ZEN_ADMIN_KEY:-test-key}"
ZEN_TEST_MODEL="${ZEN_TEST_MODEL:-deepseek-v4-flash}"
OUT_DIR="${OUT_DIR:-test-records/runs}"

ZEN_ADMIN_BASE_URLS=()
EXTRA_ARGS=()

while [[ $# -gt 0 ]]; do
  case "$1" in
    --newapi-base-url)
      NEWAPI_BASE_URL="$2"
      shift 2
      ;;
    --zen-base-url)
      ZEN_BASE_URL="$2"
      shift 2
      ;;
    --zen-admin-base-url)
      ZEN_ADMIN_BASE_URLS+=("$2")
      shift 2
      ;;
    --admin-key)
      ZEN_ADMIN_KEY="$2"
      shift 2
      ;;
    --model)
      ZEN_TEST_MODEL="$2"
      shift 2
      ;;
    --out-dir)
      OUT_DIR="$2"
      shift 2
      ;;
    *)
      EXTRA_ARGS+=("$1")
      shift
      ;;
  esac
done

if [[ ${#ZEN_ADMIN_BASE_URLS[@]} -eq 0 ]]; then
  ZEN_ADMIN_BASE_URLS=("$ZEN_BASE_URL")
fi

admin_args=()
for url in "${ZEN_ADMIN_BASE_URLS[@]}"; do
  admin_args+=(--zen-admin-base-url "$url")
done

require_key() {
  if [[ -z "${NEWAPI_API_KEY:-}" ]]; then
    echo "NEWAPI_API_KEY is required for mode '$MODE'." >&2
    exit 2
  fi
}

print_plan() {
  cat <<EOF
ZenProxyRS V4.5/P8 acceptance runner

mode:             $MODE
newapi_base_url:  $NEWAPI_BASE_URL
zen_base_url:     $ZEN_BASE_URL
zen_admin_urls:   ${ZEN_ADMIN_BASE_URLS[*]}
model:            $ZEN_TEST_MODEL
out_dir:          $OUT_DIR

Available modes:
  plan      Print this plan and run script syntax checks only.
  collect   Collect a redacted ZenProxy evidence package, no client call.
  smoke     Run NewAPI chain smoke and collect evidence.
  clients   Run Hermes/OpenClaw acceptance runner in execute mode.
  ttft      Run guarded TTFT experiment. Pass extra run_ttft_experiment.py args after mode.

Examples:
  NEWAPI_API_KEY=sk-dev scripts/run_v45_p8_acceptance.sh smoke
  NEWAPI_API_KEY=sk-dev scripts/run_v45_p8_acceptance.sh clients
  NEWAPI_API_KEY=sk-dev scripts/run_v45_p8_acceptance.sh ttft --tokens 1000
EOF
}

case "$MODE" in
  plan)
    print_plan
    python3 -m py_compile \
      scripts/collect_test_record.py \
      scripts/run_chain_smoke.py \
      scripts/run_openclaw_hermes_acceptance.py \
      scripts/run_ttft_experiment.py
    ;;
  collect)
    python3 scripts/collect_test_record.py \
      --scenario v45-p8-collect \
      --zen-base-url "$ZEN_BASE_URL" \
      "${admin_args[@]}" \
      --newapi-base-url "$NEWAPI_BASE_URL" \
      --admin-key "$ZEN_ADMIN_KEY" \
      --out-dir "$OUT_DIR" \
      "${EXTRA_ARGS[@]}"
    ;;
  smoke)
    require_key
    python3 scripts/run_chain_smoke.py \
      --scenario v45-p8-smoke \
      --newapi-base-url "$NEWAPI_BASE_URL" \
      --newapi-key "$NEWAPI_API_KEY" \
      --zen-base-url "$ZEN_BASE_URL" \
      "${admin_args[@]}" \
      --admin-key "$ZEN_ADMIN_KEY" \
      --model "$ZEN_TEST_MODEL" \
      --out-dir "$OUT_DIR" \
      "${EXTRA_ARGS[@]}"
    ;;
  clients)
    require_key
    python3 scripts/run_openclaw_hermes_acceptance.py \
      --execute \
      --base-url "$NEWAPI_BASE_URL" \
      --model "$ZEN_TEST_MODEL" \
      --out-dir "$OUT_DIR" \
      "${EXTRA_ARGS[@]}"
    ;;
  ttft)
    require_key
    python3 scripts/run_ttft_experiment.py \
      --base-url "$NEWAPI_BASE_URL" \
      --model "$ZEN_TEST_MODEL" \
      --out-dir "$OUT_DIR" \
      "${EXTRA_ARGS[@]}"
    ;;
  *)
    echo "unknown mode: $MODE" >&2
    print_plan >&2
    exit 2
    ;;
esac
