#!/usr/bin/env python3
"""Patch panda env for full free model discovery and rolling restart @1/@2/@3."""
from __future__ import annotations

import subprocess
import sys

SSH = ["ssh", "-o", "BatchMode=yes", "panda"]


def run_remote(script: str, sudo: bool = False) -> None:
    cmd = SSH + (["sudo", "bash", "-s"] if sudo else ["bash", "-s"])
    print(f">>> {' '.join(cmd)}")
    proc = subprocess.run(
        cmd,
        input=script.encode(),
        capture_output=True,
        check=False,
    )
    out = proc.stdout.decode(errors="replace")
    err = proc.stderr.decode(errors="replace")
    if out:
        print(out, end="" if out.endswith("\n") else "\n")
    if err:
        print(err, file=sys.stderr, end="" if err.endswith("\n") else "\n")
    if proc.returncode != 0:
        raise SystemExit(proc.returncode)


PATCH = r"""
set -euo pipefail
COMMON=/etc/zen-proxy-rs/common.env
backup=${COMMON}.bak-$(date +%Y%m%d-%H%M%S)-free-models
sudo cp -a "$COMMON" "$backup"
echo BACKUP=$backup

upsert() {
  file=$1 key=$2 val=$3
  if sudo grep -q "^${key}=" "$file" 2>/dev/null; then
    sudo sed -i "s|^${key}=.*|${key}=${val}|" "$file"
  else
    echo "${key}=${val}" | sudo tee -a "$file" >/dev/null
  fi
}

upsert "$COMMON" ZEN_PROVIDER_MODE free_model_kernel
upsert "$COMMON" DYNAMIC_MODEL_DISCOVERY_ENABLED true
upsert "$COMMON" DYNAMIC_MODEL_PUBLIC_MODE candidate_canary_or_active
sudo sed -i '/^DYNAMIC_MODEL_PUBLIC_ALLOWLIST=/d' "$COMMON"

echo '--- common.env ---'
sudo grep -E '^(ZEN_PROVIDER_MODE|DYNAMIC_MODEL_)' "$COMMON" || true

for inst in 1 2 3; do
  f="/etc/zen-proxy-rs/instances/${inst}.env"
  if [[ -f $f ]]; then
    upsert "$f" DYNAMIC_MODEL_DISCOVERY_ENABLED true
    upsert "$f" DYNAMIC_MODEL_PUBLIC_MODE candidate_canary_or_active
    sudo sed -i '/^DYNAMIC_MODEL_PUBLIC_ALLOWLIST=/d' "$f" || true
    echo "--- instance-${inst}.env ---"
    sudo grep -E '^(ZEN_PROVIDER_MODE|DYNAMIC_MODEL_)' "$f" || true
  fi
done
"""

ROLLING = r"""
set -euo pipefail
for entry in 1:4001 2:4002 3:4004; do
  inst=${entry%%:*}
  port=${entry##*:}
  echo "restarting zen-proxy-rs@${inst} port ${port}"
  systemctl restart "zen-proxy-rs@${inst}"
  ok=0
  for i in $(seq 1 40); do
    if curl -sf "http://127.0.0.1:${port}/health" >/dev/null; then
      ok=1
      echo "healthy @${inst} attempt=${i}"
      break
    fi
    sleep 2
  done
  [[ "$ok" == 1 ]]
  sleep 3
done
for port in 4000 4001 4002 4004; do
  code=$(curl -sf -o /dev/null -w '%{http_code}' "http://127.0.0.1:${port}/health" || echo fail)
  echo "health port ${port}: ${code}"
  [[ "$code" == 200 ]]
done
echo rolling_restart_done
"""

VERIFY = r"""
set -euo pipefail
KEY=$(grep -m1 '^PROXY_API_KEY=' /etc/zen-proxy-rs/common.env | cut -d= -f2- || true)
if [[ -n "$KEY" ]]; then
  curl -fsS -H "Authorization: Bearer ${KEY}" http://127.0.0.1:4004/v1/models
else
  curl -fsS http://127.0.0.1:4004/v1/models
fi | python3 -c "import sys,json; d=json.load(sys.stdin); ids=sorted(m['id'] for m in d.get('data',[])); print('count', len(ids)); print('\\n'.join(ids))"
"""


def main() -> int:
    print("=== PATCH ENV ===")
    run_remote(PATCH)
    print("=== ROLLING RESTART ===")
    run_remote(ROLLING, sudo=True)
    print("=== MODELS :4004 ===")
    run_remote(VERIFY)
    print("=== DONE ===")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
