#!/bin/bash
# Test if the upstream opencode AI API is reachable through webshare proxy
TEST_URL="https://httpbin.org/ip"

# 1. Test direct connectivity to webshare
echo "=== Direct webshare SOCKS5 test (first node) ==="
NODE=$(cat ./nodes.json | head -1)
echo "Node: $NODE"
timeout 10 curl -v -x "$NODE" --socks5-hostname "$NODE" "$TEST_URL" 2>&1 | head -20

echo "\n=== Upstream base connectivity through first node ==="
UPSTREAM="https://opencode.ai/zen/v1/models"
timeout 15 curl -s -x "$NODE" "$UPSTREAM" -H 'Content-Type: application/json' 2>&1 | head -30
