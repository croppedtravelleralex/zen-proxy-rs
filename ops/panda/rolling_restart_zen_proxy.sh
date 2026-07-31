#!/usr/bin/env bash
# Rolling restart zen-proxy-rs@1/@2/@3 with health checks.
# Intended for cron: Mon/Wed/Fri 06:00 Beijing time (Asia/Shanghai).
set -euo pipefail

LOG_TAG="zen-proxy-rolling-restart"
INSTANCES=("1:4001" "2:4002" "3:4004")
LOCK_FILE="/var/run/zen-proxy-rolling-restart.lock"
HEALTH_ATTEMPTS=40
HEALTH_INTERVAL_SECS=2
POST_HEALTH_SLEEP_SECS=3

log() {
  echo "$(date -Is) [$LOG_TAG] $*"
}

restart_instance() {
  local inst=$1
  local port=$2
  log "restarting zen-proxy-rs@${inst} (port ${port})"
  systemctl restart "zen-proxy-rs@${inst}"

  local attempt
  for attempt in $(seq 1 "$HEALTH_ATTEMPTS"); do
    if curl -sf "http://127.0.0.1:${port}/health" >/dev/null 2>&1; then
      local pid rss_mb
      pid=$(systemctl show -p MainPID --value "zen-proxy-rs@${inst}")
      rss_mb=$(awk '/^VmRSS:/ {printf "%.1f", $2/1024}' "/proc/${pid}/status" 2>/dev/null || echo "?")
      log "healthy @${inst} pid=${pid} rss=${rss_mb}MiB attempt=${attempt}"
      sleep "$POST_HEALTH_SLEEP_SECS"
      return 0
    fi
    sleep "$HEALTH_INTERVAL_SECS"
  done

  log "ERROR: zen-proxy-rs@${inst} failed health check on port ${port}"
  systemctl status "zen-proxy-rs@${inst}" --no-pager -l | tail -15 >&2 || true
  return 1
}

main() {
  exec 9>"$LOCK_FILE"
  if ! flock -n 9; then
    log "skip: another rolling restart is in progress"
    exit 0
  fi

  log "start"
  free -h | awk '/Mem:|Swap:/ {print}'

  local entry inst port
  for entry in "${INSTANCES[@]}"; do
    inst="${entry%%:*}"
    port="${entry##*:}"
    restart_instance "$inst" "$port"
  done

  for port in 4000 4001 4002 4004; do
    local code
    code=$(curl -sf -o /dev/null -w '%{http_code}' "http://127.0.0.1:${port}/health" 2>/dev/null || echo "fail")
    log "balancer health port ${port}: ${code}"
    if [ "$code" != "200" ]; then
      log "ERROR: port ${port} health check failed"
      exit 1
    fi
  done

  log "done"
}

main "$@"
