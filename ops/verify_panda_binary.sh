#!/usr/bin/env bash
set -euo pipefail
ssh -o BatchMode=yes panda 'sha256sum /opt/zen-proxy-rs/zen-proxy-rs'
ssh -o BatchMode=yes panda 'strings /opt/zen-proxy-rs/zen-proxy-rs | grep -m1 usk_v1 || echo NO_USK_STRING'
ssh -o BatchMode=yes panda 'tail -1 /var/log/zen-proxy-rs/audit/requests-2026-07-03.jsonl' | head -c 500
echo
