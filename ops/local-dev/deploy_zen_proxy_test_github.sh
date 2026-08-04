#!/usr/bin/env bash
# Deploy zen-proxy-rs-test (:4011) via temporary private GitHub release (no scp).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../" && pwd)"
STAMP="${1:-test-$(date +%Y%m%d-%H%M%S)}"
SSH_HOST="${SSH_HOST:-panda}"
BINARY_NAME="zen-proxy-rs.${STAMP}"
LOCAL_BIN="${ROOT}/repos/zen-proxy-rs/target/release/zen-proxy-rs"
REMOTE_DIR="/opt/zen-proxy-rs"
ARTIFACT_REPO="${ARTIFACT_REPO:-croppedtravelleralex/zen-proxy-rs-artifacts}"
RELEASE_TAG="zen-proxy-${STAMP}"
GH="${GH_CLI:-/mnt/c/Program Files/GitHub CLI/gh.exe}"

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
  --notes "Temporary test deploy for zen-proxy-rs-test (${STAMP}). Delete after pull." \
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

ssh -o BatchMode=yes "${SSH_HOST}" "bash -s -- '${ASSET_API_URL}' '${LOCAL_SHA}' '${STAMP}' '${BINARY_NAME}' '/tmp/zen-github-token.${STAMP}'" <<'REMOTE'
set -euo pipefail
asset_api_url=$1
expected_sha=$2
stamp=$3
binary_name=$4
token_file=$5
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

current=$(systemctl show -p ExecStart --value zen-proxy-rs-test | awk '{print $1}')
if [[ -n "$current" && -x "$current" ]]; then
  cp --preserve=mode,timestamps "$current" "${backup_dir}/$(basename "$current").bak-${stamp}"
fi

override_dir=/etc/systemd/system/zen-proxy-rs-test.service.d
mkdir -p "$override_dir"
cat > "${override_dir}/deploy.conf" <<EOF
[Service]
ExecStart=
ExecStart=${download}
EOF
systemctl daemon-reload
systemctl restart zen-proxy-rs-test
sleep 3
active=$(systemctl is-active zen-proxy-rs-test)
health_code=$(curl -fsS -o /tmp/zen-health-4011.json -w '%{http_code}' http://127.0.0.1:4011/health)
health=$(cat /tmp/zen-health-4011.json)
echo "DEPLOY active=$active health_code=$health_code health=$health"
test "$active" = active
test "$health_code" = 200
python3 - <<'PY'
import json
h=json.load(open("/tmp/zen-health-4011.json"))
assert h.get("status") == "ok"
print("git_hash=", h.get("git_hash"), "build_time=", h.get("build_time"))
PY
REMOTE

trap - EXIT
cleanup_github
echo "DONE stamp=${STAMP} sha256=${LOCAL_SHA} via GitHub release ${RELEASE_TAG}"
