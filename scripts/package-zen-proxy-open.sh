#!/usr/bin/env bash
# Rebuild the open-source standalone package from the monorepo.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="$ROOT/dist/zen-proxy-rs"
SRC_PROXY="$ROOT/repos/zen-proxy-rs"
SRC_CLIENT="$ROOT/repos/free-model-client-rs"

mkdir -p "$OUT/zen-proxy-rs" "$OUT/free-model-client-rs"

rsync -a --delete --exclude target --exclude docs --exclude '.env' --exclude nodes.json \
  "$SRC_PROXY/src" "$SRC_PROXY/tests" "$SRC_PROXY/Cargo.toml" "$SRC_PROXY/Cargo.lock" \
  "$SRC_PROXY/build.rs" "$SRC_PROXY/.gitignore" "$SRC_PROXY/.gitattributes" \
  "$OUT/zen-proxy-rs/"

rsync -a --delete --exclude target \
  "$SRC_CLIENT/src" "$SRC_CLIENT/Cargo.toml" "$SRC_CLIENT/Cargo.lock" \
  "$SRC_CLIENT/.gitignore" "$SRC_CLIENT/README.md" "$SRC_CLIENT/.env.example" \
  "$OUT/free-model-client-rs/"

rm -f "$OUT/zen-proxy-rs/src/config.rs.bak" \
      "$OUT/zen-proxy-rs/test_openapi.sh" \
      "$OUT/zen-proxy-rs/tests/v45_p8_acceptance_matrix.md"

# Restore open-source overlay files (README, LICENSE, Docker, etc.)
# Run this script from a checkout that already has dist/zen-proxy-rs overlay,
# or copy overlay files manually after first creation.

echo "Packaged to $OUT"
echo "Note: overlay files (README, LICENSE, Dockerfile) are preserved if already present."
