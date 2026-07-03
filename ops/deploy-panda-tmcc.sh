#!/usr/bin/env bash
# TMCC 99+ panda rolling deploy: nginx sticky + zen-proxy-rs x3
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
ZEN_PROXY_RS="${REPO_ROOT}/repos/zen-proxy-rs"
STRIPPED="/tmp/zen-proxy-rs.tmcc.stripped"
STAMP="$(date +%Y%m%d-%H%M%S)"
PORTS=(1:4001 2:4002 3:4004)

OLD_HASH="$(ssh panda 'sha256sum /opt/zen-proxy-rs/zen-proxy-rs' | awk '{print $1}')"

echo "==> build release"
cd "${ZEN_PROXY_RS}"
CARGO_INCREMENTAL=0 cargo build --release
strip -o "${STRIPPED}" target/release/zen-proxy-rs
NEW_HASH="$(sha256sum "${STRIPPED}" | awk '{print $1}')"
echo "old=${OLD_HASH}"
echo "new=${NEW_HASH}"

echo "==> upload binary"
scp "${STRIPPED}" "panda:/tmp/zen-proxy-rs.tmcc.${STAMP}"

ssh panda bash -s <<EOF
set -euo pipefail
install -d /opt/zen-proxy-rs/backups
cp -a /opt/zen-proxy-rs/zen-proxy-rs "/opt/zen-proxy-rs/backups/zen-proxy-rs.${STAMP}.pre-tmcc-${OLD_HASH:0:12}"
install -m 755 "/tmp/zen-proxy-rs.tmcc.${STAMP}" /opt/zen-proxy-rs/zen-proxy-rs
sha256sum /opt/zen-proxy-rs/zen-proxy-rs
EOF

echo "==> deploy nginx sticky"
scp "${REPO_ROOT}/ops/zen-balancer-sticky.conf" "panda:/tmp/zen-balancer.tmcc.${STAMP}"
ssh panda bash -s <<EOF
set -euo pipefail
cp -a /etc/nginx/sites-available/zen-balancer "/etc/nginx/sites-available/zen-balancer.bak.${STAMP}"
install -m 644 "/tmp/zen-balancer.tmcc.${STAMP}" /etc/nginx/sites-available/zen-balancer
nginx -t
systemctl reload nginx
grep -E 'hash|least_conn|4001' /etc/nginx/sites-available/zen-balancer
EOF

for entry in "${PORTS[@]}"; do
  inst="${entry%%:*}"
  port="${entry##*:}"
  echo "==> restart zen-proxy-rs@${inst}"
  ssh panda bash -s <<EOF
set -euo pipefail
# CCP / TMCC 2.0 feature flags (override in unit drop-in if needed)
for f in /etc/default/zen-proxy-rs@${inst} /etc/zen-proxy-rs/instance-${inst}.env; do
  [ -f "\$f" ] || continue
  grep -q CCP_ICP_ENABLED "\$f" || echo 'CCP_ICP_ENABLED=1' >> "\$f"
  grep -q CCP_PROMPT_CACHE_KEY "\$f" || echo 'CCP_PROMPT_CACHE_KEY=1' >> "\$f"
  grep -q CCP_REASONING_SIDECAR "\$f" || echo 'CCP_REASONING_SIDECAR=1' >> "\$f"
  grep -q CCP_TRF_STRICT "\$f" || echo 'CCP_TRF_STRICT=1' >> "\$f"
done
systemctl restart zen-proxy-rs@${inst}
EOF
  for _ in $(seq 1 30); do
    if ssh panda "curl -sf http://127.0.0.1:${port}/health" >/dev/null; then
      echo "zen-proxy-rs@${inst} healthy on ${port}"
      break
    fi
    sleep 2
  done
done

echo "==> smoke"
ssh panda bash -s <<'SMOKE'
set -euo pipefail
curl -sf http://127.0.0.1:4000/health
echo
curl -sf http://127.0.0.1:4000/v1/models | head -c 500
echo
SMOKE

echo "deploy complete stamp=${STAMP}"
