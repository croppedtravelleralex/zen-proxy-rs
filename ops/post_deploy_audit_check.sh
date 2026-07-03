#!/usr/bin/env bash
set -euo pipefail
AUDIT_LOCAL="/tmp/panda-audit-post.jsonl"
scp -o BatchMode=yes panda:/var/log/zen-proxy-rs/audit/requests-$(date +%Y-%m-%d).jsonl "$AUDIT_LOCAL"
python3 "$(dirname "$0")/post_deploy_audit_sample.py" "$AUDIT_LOCAL"
echo "--- acceptance ---"
python3 "$(dirname "$0")/cache_quality_acceptance.py" "$AUDIT_LOCAL" 2>&1 | tail -10
