#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../" && pwd)"
STAMP="${1:-test-20260804-frt-v7}"
LOG="${ROOT}/.local-dev/frt-v7-pipeline.log"

export PATH="${HOME}/.cargo/bin:${PATH}"
[[ -f "${HOME}/.cargo/env" ]] && source "${HOME}/.cargo/env"

{
  echo "=== frt-v7 pipeline $(date -Is) stamp=$STAMP ==="
  cd "${ROOT}/repos/zen-proxy-rs"
  cargo test stream_precheck_forwards_rate_limit_error_within_budget -q
  cargo test stream_metrics_thinking_only_counts_as_assistant_output -q
  cargo test stream_metrics_detects_anthropic_rate_limit_error_event -q
  cargo build --release -q
  sha256sum target/release/zen-proxy-rs
  cd "${ROOT}"
  bash ops/local-dev/deploy_zen_proxy_test_github.sh "$STAMP"
  ssh -o BatchMode=yes panda curl -sf http://127.0.0.1:4011/health
  echo
  echo "=== PIPELINE_END $(date -Is) ==="
} 2>&1 | tee -a "$LOG"
