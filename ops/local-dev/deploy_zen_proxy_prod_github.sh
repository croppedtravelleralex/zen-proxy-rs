#!/usr/bin/env bash
# Deploy ONE production zen-proxy instance via temporary GitHub release (no scp binary).
# Usage: deploy_zen_proxy_prod_github.sh STAMP INSTANCE
#   INSTANCE: 1 -> :4001 zen-proxy-rs@1
#             2 -> :4002 zen-proxy-rs@2
#             3 -> :4004 zen-proxy-rs@3  (default canary)
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../" && pwd)"
STAMP="${1:-prod-$(date +%Y%m%d-%H%M%S)-frt-v8}"
INSTANCE="${2:-3}"
SSH_HOST="${SSH_HOST:-panda}"
BINARY_NAME="zen-proxy-rs.${STAMP}"
LOCAL_BIN="${ROOT}/repos/zen-proxy-rs/target/release/zen-proxy-rs"
REMOTE_DIR="/opt/zen-proxy-rs"
ARTIFACT_REPO="${ARTIFACT_REPO:-croppedtravelleralex/zen-proxy-rs-artifacts}"
RELEASE_TAG="zen-proxy-${STAMP}"
GH="${GH_CLI:-/mnt/c/Program Files/GitHub CLI/gh.exe}"

case "$INSTANCE" in
  1) SERVICE="zen-proxy-rs@1"; PORT=4001 ;;
  2) SERVICE="zen-proxy-rs@2"; PORT=4002 ;;
  3) SERVICE="zen-proxy-rs@3"; PORT=4004 ;;
  *)
    echo "INSTANCE must be 1, 2, or 3 (got: $INSTANCE)" >&2
    exit 1
    ;;
esac

if [[ ! -x "$GH" ]]; then
  echo "gh CLI not found at $GH (set GH_CLI)" >&2
  exit 1
fi

echo "BUILD release binary..."
(cd "${ROOT}/repos/zen-proxy-rs" && cargo build --release)
LOCAL_SHA="$(sha256sum "$LOCAL_BIN" | awk '{print $1}')"
sha256sum "$LOCAL_BIN"

echo "GITHUB release ${ARTIFACT_REPO} tag=${RELEASE_TAG}"
"$GH" release create "$RELEASE_TAG" \
  --repo "$ARTIFACT_REPO" \
  --title "$STAMP" \
  --notes "Production canary deploy ${SERVICE} :${PORT} (${STAMP}). Delete after pull." \
  "$LOCAL_BIN#zen-proxy-rs"

ASSET_API_URL="$("$GH" api "/repos/${ARTIFACT_REPO}/releases/tags/${RELEASE_TAG}" --jq '.assets[0].url')"
if [[ -z "$ASSET_API_URL" || "$ASSET_API_URL" == "null" ]]; then
  echo "failed to resolve release asset API URL" >&2
  exit 1
fi
echo "ASSET_API_URL=$ASSET_API_URL"

GITHUB_TOKEN="$("$GH" auth token)"
TOKEN_FILE="$(mktemp)"
chmod 600 "$TOKEN_FILE"
printf '%s' "$GITHUB_TOKEN" >"$TOKEN_FILE"

cleanup_github() {
  rm -f -- "$TOKEN_FILE"
  "$GH" release delete "$RELEASE_TAG" --repo "$ARTIFACT_REPO" --yes >/dev/null 2>&1 || true
}
trap cleanup_github EXIT

scp -o BatchMode=yes "$TOKEN_FILE" "${SSH_HOST}:/tmp/zen-github-token.${STAMP}"

ssh -o BatchMode=yes "${SSH_HOST}" "bash -s -- '${ASSET_API_URL}' '${LOCAL_SHA}' '${STAMP}' '${BINARY_NAME}' '/tmp/zen-github-token.${STAMP}' '${SERVICE}' '${PORT}'" <<'REMOTE'
set -euo pipefail
asset_api_url=$1
expected_sha=$2
stamp=$3
binary_name=$4
token_file=$5
service=$6
port=$7
download="/opt/zen-proxy-rs/${binary_name}"
backup_dir="/opt/zen-proxy-rs/backups"
mkdir -p "$backup_dir"
rm -f -- "$download"

curl --fail --location --silent --show-error \
  -H "Authorization: Bearer $(cat "$token_file")" \
  -H "Accept: application/octet-stream" \
  "$asset_api_url" -o "$download"
chmod 0755 "$download"
rm -f -- "$token_file"

actual_sha=$(sha256sum "$download" | awk '{print $1}')
echo "DOWNLOAD sha256=$actual_sha size=$(stat -c %s "$download")"
test "$actual_sha" = "$expected_sha"

current=$(systemctl show -p ExecStart --value "$service" | awk '{print $1}')
if [[ -n "$current" && -x "$current" ]]; then
  cp --preserve=mode,timestamps "$current" "${backup_dir}/$(basename "$current").bak-${stamp}"
  echo "BACKUP=${backup_dir}/$(basename "$current").bak-${stamp}"
fi

override_dir="/etc/systemd/system/${service}.service.d"
mkdir -p "$override_dir"
cat > "${override_dir}/deploy.conf" <<EOF
[Service]
ExecStart=
ExecStart=${download}
EOF
systemctl daemon-reload
systemctl restart "$service"
sleep 4
active=$(systemctl is-active "$service")
health_code=$(curl -fsS -o "/tmp/zen-health-${port}.json" -w '%{http_code}' "http://127.0.0.1:${port}/health")
health=$(cat "/tmp/zen-health-${port}.json")
echo "DEPLOY service=$service port=$port active=$active health_code=$health_code health=$health"
test "$active" = active
test "$health_code" = 200
python3 -c "
import json
h=json.load(open('/tmp/zen-health-${port}.json'))
assert h.get('status') == 'ok'
print('git_hash=', h.get('git_hash'), 'build_time=', h.get('build_time'))
"
REMOTE

trap - EXIT
cleanup_github
echo "DONE stamp=${STAMP} instance=${INSTANCE} service=${SERVICE} port=${PORT} sha256=${LOCAL_SHA}"
