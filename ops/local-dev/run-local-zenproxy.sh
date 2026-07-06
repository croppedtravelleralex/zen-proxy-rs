#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
DEV_DIR="${ROOT}/ops/local-dev"
DATA_DIR="${ROOT}/.local-dev"
ENV_FILE="${DEV_DIR}/local.env"

if [[ ! -f "$ENV_FILE" ]]; then
  cp "${DEV_DIR}/local.env.example" "$ENV_FILE"
  echo "Created $ENV_FILE from example"
fi

PRESET_PROXY_API_KEY="${PROXY_API_KEY:-}"

# shellcheck disable=SC1090
set -a
source "$ENV_FILE"
set +a

if [[ -n "$PRESET_PROXY_API_KEY" ]]; then
  export PROXY_API_KEY="$PRESET_PROXY_API_KEY"
fi

mkdir -p "${AUDIT_LOG_DIR}" "$(dirname "${LEDGER_EVENTS_PATH}")"

if [[ ! -f "${NODES_FILE}" ]]; then
  echo "Missing ${NODES_FILE}; run bash ops/local-dev/01_preflight.sh first"
  exit 1
fi

ZP="${ROOT}/repos/zen-proxy-rs/target/release/zen-proxy-rs"
if [[ ! -x "$ZP" ]]; then
  echo "Building zen-proxy-rs release..."
  (cd "${ROOT}/repos/zen-proxy-rs" && cargo build --release)
fi

echo "Starting local zen-proxy-rs on ${BIND_ADDRESS}:${PORT}"
echo "Audit: ${AUDIT_LOG_DIR}"
exec "$ZP"
