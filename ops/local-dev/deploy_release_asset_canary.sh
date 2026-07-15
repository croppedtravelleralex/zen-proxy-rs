#!/usr/bin/env bash
set -euo pipefail

asset_api_url=${1:?asset API URL required}
expected_sha256=${2:?expected SHA256 required}
stamp=${3:?deployment stamp required}
token_file=${4:?remote token file required}

ssh -o BatchMode=yes panda "bash -s -- '$asset_api_url' '$expected_sha256' '$stamp' '$token_file'" <<'REMOTE'
set -euo pipefail

asset_api_url=$1
expected_sha256=$2
stamp=$3
token_file=$4
download="/tmp/zen-proxy-rs.${stamp}"
backup="/opt/zen-proxy-rs/zen-proxy-rs.pre-${stamp}"

cleanup() {
  rm -f -- "$token_file" "$download"
}
rollback_needed=0
canary_verified=0
rollback_canary() {
  if (( rollback_needed == 1 && canary_verified == 0 )) && [[ -s "$backup" ]]; then
    echo "ROLLBACK restoring=$backup" >&2
    install -m 0755 "$backup" /opt/zen-proxy-rs/zen-proxy-rs
    systemctl daemon-reload
    systemctl restart zen-proxy-rs@1
  fi
}
finish() {
  status=$?
  if (( status != 0 )); then
    rollback_canary || true
  fi
  cleanup
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
if (( available_mb < 1024 )); then
  echo "preflight failed: low available memory" >&2
  exit 20
fi
if awk -v pct="$available_pct" 'BEGIN{exit !(pct < 25.0)}'; then
  echo "preflight failed: available memory below 25 percent" >&2
  exit 21
fi
if awk -v load_value="$normalized_load" 'BEGIN{exit !(load_value > 0.70)}'; then
  echo "preflight failed: normalized load too high" >&2
  exit 22
fi
if (( root_disk_pct > 85 )); then
  echo "preflight failed: root disk above 85 percent" >&2
  exit 23
fi

for port in 4000 4001 4002 4004; do
  test "$(curl -fsS -o /dev/null -w '%{http_code}' "http://127.0.0.1:${port}/health")" = 200
done
test -s "$token_file"

curl --fail --location --silent --show-error \
  -H "Authorization: Bearer $(cat "$token_file")" \
  -H "Accept: application/octet-stream" \
  "$asset_api_url" -o "$download"
actual_sha256=$(sha256sum "$download" | awk '{print $1}')
echo "ASSET sha256=$actual_sha256 size=$(stat -c %s "$download")"
test "$actual_sha256" = "$expected_sha256"

cp --preserve=mode,timestamps /opt/zen-proxy-rs/zen-proxy-rs "$backup"
install -m 0755 "$download" /opt/zen-proxy-rs/zen-proxy-rs.new
mv /opt/zen-proxy-rs/zen-proxy-rs.new /opt/zen-proxy-rs/zen-proxy-rs
rollback_needed=1
systemctl daemon-reload
systemctl restart zen-proxy-rs@1
sleep 3

pid=$(systemctl show -p MainPID --value zen-proxy-rs@1)
active=$(systemctl is-active zen-proxy-rs@1)
health=$(curl -fsS -o /dev/null -w '%{http_code}' http://127.0.0.1:4001/health)
exe_sha256=$(sha256sum "/proc/${pid}/exe" | awk '{print $1}')
echo "CANARY instance=1 active=$active health=$health pid=$pid exe_sha256=$exe_sha256 backup=$backup"
test "$active" = active
test "$health" = 200
test "$exe_sha256" = "$expected_sha256"
post_available_mb=$(free -m | awk '/Mem:/{print $7}')
post_load1=$(awk '{print $1}' /proc/loadavg)
post_normalized_load=$(awk -v load_value="$post_load1" -v cpus="$cpus" 'BEGIN{printf "%.3f", load_value/cpus}')
echo "POSTCHECK mem_available_mb=$post_available_mb normalized_load=$post_normalized_load"
(( post_available_mb >= 768 ))
awk -v load_value="$post_normalized_load" 'BEGIN{exit !(load_value <= 1.0)}'
canary_verified=1
REMOTE
