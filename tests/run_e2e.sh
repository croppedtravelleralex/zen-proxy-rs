#!/bin/bash
# tests/run_e2e.sh - E2E integration tests
set -e

PORT=19995
BIND_ADDRESS=127.0.0.1
ADMIN_API_KEY=test-key-123

export PORT="${PORT}"
export BIND_ADDRESS="${BIND_ADDRESS}"
export PROXY_TOKEN_MODE=unlimited
export ADMIN_API_KEY="${ADMIN_API_KEY}"
export NODES_FILE=/dev/null

# Kill any stale server
pkill -f "zen-proxy-rs" 2>/dev/null || true
sleep 0.5

# Build
cargo +nightly build -q 2>/dev/null

# Start server in background
./target/debug/zen-proxy-rs &
SERVER_PID=$!
sleep 0.5

PASS=0
FAIL=0

check() {
    local name="$1"
    local expected="$2"
    local actual="$3"
    if echo "$actual" | grep -q "$expected"; then
        echo "PASS: $name"
        PASS=$((PASS+1))
    else
        echo "FAIL: $name (expected: $expected)"
        echo "       got: $actual"
        FAIL=$((FAIL+1))
    fi
}

cleanup() {
    kill $SERVER_PID 2>/dev/null || true
    wait $SERVER_PID 2>/dev/null || true
    unset PORT BIND_ADDRESS PROXY_TOKEN_MODE ADMIN_API_KEY NODES_FILE
}
trap cleanup EXIT

# Test 1: health
resp=$(curl -s http://${BIND_ADDRESS}:${PORT}/health)
check "health status" '"status":"ok"' "$resp"
check "health version" '"version"' "$resp"
check "health uptime" '"uptime_secs"' "$resp"

# Test 2: metrics
resp=$(curl -s http://${BIND_ADDRESS}:${PORT}/metrics)
check "metrics total" 'zen_proxy_requests_total' "$resp"
check "metrics rpm" 'zen_proxy_rpm' "$resp"

# Test 3: index
resp=$(curl -s http://${BIND_ADDRESS}:${PORT}/)
check "index service" '"zen-proxy-rs"' "$resp"

# Test 4: admin auth (no key = 401)
code=$(curl -s -o /dev/null -w "%{http_code}" http://${BIND_ADDRESS}:${PORT}/admin/stats)
check "admin auth rejects no key" '401' "$code"

# Test 5: admin auth (with key)
code=$(curl -s -o /dev/null -w "%{http_code}" -H "x-api-key: ${ADMIN_API_KEY}" http://${BIND_ADDRESS}:${PORT}/admin/stats)
check "admin auth accepts valid key" '200' "$code"

# Test 6: admin stats with key
resp=$(curl -s -H "x-api-key: ${ADMIN_API_KEY}" http://${BIND_ADDRESS}:${PORT}/admin/stats)
check "admin stats" '"requests"' "$resp"

echo "---"
echo "PASS: $PASS, FAIL: $FAIL"
if [ $FAIL -gt 0 ]; then exit 1; fi
