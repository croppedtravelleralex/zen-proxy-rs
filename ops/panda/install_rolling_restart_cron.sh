#!/usr/bin/env bash
# Install rolling restart script and cron on panda (Mon/Wed/Fri 06:00 Beijing time).
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
REMOTE_SCRIPT="/opt/zen-proxy-rs/rolling_restart.sh"
REMOTE_CRON="/etc/cron.d/zen-proxy-rolling-restart"
LOCAL_SCRIPT="${REPO_ROOT}/ops/panda/rolling_restart_zen_proxy.sh"

ssh -o BatchMode=yes -o ConnectTimeout=15 panda 'install -d /opt/zen-proxy-rs /var/log/zen-proxy-rs'
scp -o BatchMode=yes "$LOCAL_SCRIPT" "panda:${REMOTE_SCRIPT}"

ssh -o BatchMode=yes -o ConnectTimeout=15 panda bash -s <<REMOTE
set -euo pipefail
chmod 755 "${REMOTE_SCRIPT}"
cat > "${REMOTE_CRON}" <<CRON
# Rolling restart zen-proxy-rs instances to reclaim heap fragmentation.
# Mon/Wed/Fri at 06:00 Beijing time (Asia/Shanghai).
SHELL=/bin/bash
PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
0 6 * * 1,3,5 root ${REMOTE_SCRIPT} >> /var/log/zen-proxy-rs/rolling-restart.log 2>&1
CRON
chmod 644 "${REMOTE_CRON}"
systemctl status cron >/dev/null 2>&1 || systemctl status crond >/dev/null 2>&1
echo "installed:"
ls -la "${REMOTE_SCRIPT}" "${REMOTE_CRON}"
echo "--- cron ---"
cat "${REMOTE_CRON}"
REMOTE

echo "install complete"
