#!/usr/bin/env bash
set -euo pipefail

expected_sha256=${1:?expected SHA256 required}
backup=${2:?canary backup path required}

ssh -o BatchMode=yes panda "bash -s -- '$expected_sha256' '$backup'" <<'REMOTE'
set -euo pipefail

expected_sha256=$1
backup=$2
test -s "$backup"

rollout_started=0
rollout_verified=0
rollback_rollout() {
  if (( rollout_started == 1 && rollout_verified == 0 )); then
    echo "ROLLBACK restoring=$backup" >&2
    install -m 0755 "$backup" /opt/zen-proxy-rs/zen-proxy-rs
    systemctl daemon-reload
    for instance in 1 2 3; do
      systemctl restart "zen-proxy-rs@${instance}" || true
      sleep 2
    done
  fi
}
finish() {
  status=$?
  if (( status != 0 )); then
    rollback_rollout || true
  fi
  trap - EXIT
  exit "$status"
}
trap finish EXIT

available_mb=$(free -m | awk '/Mem:/{print $7}')
total_mb=$(free -m | awk '/Mem:/{print $2}')
available_pct=$(awk -v available="$available_mb" -v total="$total_mb" 'BEGIN{printf "%.1f", 100*available/total}')
load1=$(awk '{print $1}' /proc/loadavg)
cpus=$(nproc)
normalized_load=$(awk -v load_value="$load1" -v cpus="$cpus" 'BEGIN{printf "%.3f", load_value/cpus}')
root_disk_pct=$(df -P / | awk 'NR==2 {gsub(/%/, "", $5); print $5}')
echo "PREFLIGHT mem_available_mb=$available_mb mem_available_pct=$available_pct normalized_load=$normalized_load root_disk_pct=$root_disk_pct"
(( available_mb >= 1024 ))
awk -v pct="$available_pct" 'BEGIN{exit !(pct >= 25.0)}'
awk -v load_value="$normalized_load" 'BEGIN{exit !(load_value <= 0.70)}'
(( root_disk_pct <= 85 ))

rollout_started=1
for instance in 2 3; do
  systemctl restart "zen-proxy-rs@${instance}"
  sleep 3
  pid=$(systemctl show -p MainPID --value "zen-proxy-rs@${instance}")
  port=$((4000 + instance))
  if (( instance == 3 )); then
    port=4004
  fi
  active=$(systemctl is-active "zen-proxy-rs@${instance}")
  health=$(curl -fsS -o /dev/null -w '%{http_code}' "http://127.0.0.1:${port}/health")
  exe_sha256=$(sha256sum "/proc/${pid}/exe" | awk '{print $1}')
  echo "ROLLOUT instance=$instance active=$active health=$health pid=$pid exe_sha256=$exe_sha256"
  test "$active" = active
  test "$health" = 200
  test "$exe_sha256" = "$expected_sha256"
  post_available_mb=$(free -m | awk '/Mem:/{print $7}')
  post_load1=$(awk '{print $1}' /proc/loadavg)
  post_normalized_load=$(awk -v load_value="$post_load1" -v cpus="$cpus" 'BEGIN{printf "%.3f", load_value/cpus}')
  echo "POSTCHECK instance=$instance mem_available_mb=$post_available_mb normalized_load=$post_normalized_load"
  (( post_available_mb >= 768 ))
  awk -v load_value="$post_normalized_load" 'BEGIN{exit !(load_value <= 1.0)}'
done

for port in 4000 4001 4002 4004; do
  health=$(curl -fsS -o /dev/null -w '%{http_code}' "http://127.0.0.1:${port}/health")
  echo "HEALTH port=$port status=$health"
  test "$health" = 200
done
rollout_verified=1
REMOTE
