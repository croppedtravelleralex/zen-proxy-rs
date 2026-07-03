#!/usr/bin/env python3
"""部署后仍像旧版：核查二进制、env、audit schema 代际。"""
from __future__ import annotations

import json
import subprocess
from collections import Counter
from datetime import datetime, timedelta, timezone

CST = timezone(timedelta(hours=8))
DEPLOY_MS = int(datetime(2026, 7, 3, 11, 54, 0, tzinfo=CST).timestamp() * 1000)
SINCE_MS = int(datetime(2026, 7, 3, 13, 57, 0, tzinfo=CST).timestamp() * 1000)
EXPECTED_HASH_PREFIX = "572cba42"
CCP_FIELDS = (
    "usk",
    "icp_scope",
    "prefix_32k_hash",
    "session_pin_hit",
    "prompt_cache_key",
    "warmup_state",
    "provider_cache_observation",
)


def ssh(cmd: str) -> str:
    p = subprocess.run(
        ["ssh", "-o", "BatchMode=yes", "panda", cmd],
        capture_output=True,
        text=True,
    )
    return (p.stdout or "") + (p.stderr or "")


def main() -> None:
    print("=== 1. 运行中二进制与 uptime ===")
    print(ssh(
        "sha256sum /opt/zen-proxy-rs/zen-proxy-rs; "
        "for p in 4001 4002 4004; do "
        "echo -n port=$p pid=; "
        "curl -sf http://127.0.0.1:$p/health | python3 -c \"import sys,json; d=json.load(sys.stdin); print(d.get('pid'), 'uptime', d.get('uptime_secs'))\"; "
        "done"
    ))

    print("\n=== 2. ZEN_PROVIDER_MODE / CCP env（三实例）===")
    for i in (1, 2, 3):
        print(f"--- zen-proxy-rs@{i} ---")
        print(ssh(
            f"systemctl show zen-proxy-rs@{i} -p Environment 2>/dev/null; "
            f"grep -hE 'ZEN_PROVIDER|CCP_' /etc/default/zen-proxy-rs@{i} 2>/dev/null; "
            f"grep -hE 'ZEN_PROVIDER|CCP_' /etc/zen-proxy-rs/instance-{i}.env 2>/dev/null"
        ))

    print("\n=== 3. audit 行 schema 代际（13:57 后 deepseek）===")
    raw = ssh("cat /var/log/zen-proxy-rs/audit/requests-2026-07-03.jsonl")
    rows = []
    for line in raw.splitlines():
        try:
            r = json.loads(line)
        except json.JSONDecodeError:
            continue
        ts = int(r.get("ts") or 0)
        if ts < SINCE_MS:
            continue
        if "deepseek" not in str(r.get("public_model") or r.get("model") or ""):
            continue
        rows.append(r)

    def generation(r: dict) -> str:
        keys = set(r.keys())
        if "usk" in keys:
            return "new_ccp_schema"
        return "legacy_schema_no_ccp_fields"

    gen = Counter(generation(r) for r in rows)
    print(f"rows={len(rows)} generations={dict(gen)}")

    for label, pred in [
        ("new_sample", lambda r: "usk" in r),
        ("legacy_sample", lambda r: "usk" not in r),
    ]:
        sample = next((r for r in rows if pred(r)), None)
        if sample:
            print(f"\n{label} keys ({len(sample)}):", sorted(sample.keys())[-12:])
            print(
                label,
                {
                    k: sample.get(k)
                    for k in [
                        "ts",
                        "affinity_key",
                        "affinity_hit",
                        "session_pin_hit",
                        "usk",
                        "session_id",
                        "cache_read_input_tokens",
                        "prompt_tokens",
                    ]
                },
            )

    # per-port pid if encoded in node - check post-deploy only
    post_deploy = [r for r in rows if int(r.get("ts") or 0) >= DEPLOY_MS]
    post_gen = Counter(generation(r) for r in post_deploy)
    print(f"\npost_deploy_rows={len(post_deploy)} gen={dict(post_gen)}")

    print("\n=== 4. affinity_key 形态（是否 USK 路由）===")
    samples = [str(r.get("affinity_key") or "") for r in rows if r.get("affinity_key")][:5]
    for s in samples:
        print(" ", s[:120])
    has_p_prefix = sum(1 for r in rows if ":p" in str(r.get("affinity_key") or ""))
    has_claude_code_path = sum(
        1
        for r in rows
        if str(r.get("affinity_key") or "").count(":") >= 4
        and "chat" not in str(r.get("affinity_key") or "")
        and "messages" not in str(r.get("affinity_key") or "").split(":")[2:3]
    )
    print(f"old_p_prefix_count={has_p_prefix} usk_style_count={len(rows)-has_p_prefix}")

    print("\n=== 5. ccswitch cache（本机 WSL）===")
    py = f"""
import sqlite3, json
from datetime import datetime, timezone, timedelta
CST=timezone(timedelta(hours=8))
start=datetime(2026,7,3,13,57,0,tzinfo=CST).strftime('%Y-%m-%d %H:%M:%S')
conn=sqlite3.connect('/root/.cc-switch/cc-switch.db')
row=conn.execute('''
SELECT COUNT(*), SUM(input_tokens), SUM(cache_read_tokens), SUM(cache_creation_tokens)
FROM proxy_request_logs WHERE created_at>=? AND request_model LIKE '%deepseek%'
''',(start,)).fetchone()
out={{'count':row[0],'input':row[1],'cache_read':row[2],'cache_creation':row[3]}}
if row[1]: out['R1_pct']=round((row[2] or 0)/row[1]*100,2)
print(json.dumps(out))
"""
    p = subprocess.run(["python3", "-c", py], capture_output=True, text=True)
    print(p.stdout or p.stderr)


if __name__ == "__main__":
    main()
