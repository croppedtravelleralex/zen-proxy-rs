#!/usr/bin/env python3
"""NewAPI + ZenProxy 对账（修正时间窗与 cache_tokens 口径）。"""
from __future__ import annotations

import json
import subprocess
from datetime import datetime, timedelta, timezone

CST = timezone(timedelta(hours=8))
SINCE = datetime(2026, 7, 3, 13, 57, 0, tzinfo=CST)
START_S = int(SINCE.timestamp())
START_MS = START_S * 1000
MODEL = "deepseek-v4-flash"


def ssh_psql(sql: str) -> str:
    cmd = (
        f'docker exec new-api-postgres psql -U newapi -d new-api -At -c "{sql}"'
    )
    proc = subprocess.run(
        ["ssh", "-o", "BatchMode=yes", "panda", cmd],
        capture_output=True,
        text=True,
        check=False,
    )
    return proc.stdout if proc.returncode == 0 else proc.stderr


def fetch_newapi_rows() -> list[dict]:
    sql = (
        "SELECT row_to_json(t) FROM ("
        f"SELECT request_id, model_name, type, created_at, prompt_tokens, completion_tokens, other::text AS other "
        f"FROM logs WHERE created_at >= {START_S} AND channel_id = 69 "
        f"AND model_name ILIKE '%deepseek%' ORDER BY created_at"
        ") t;"
    )
    out = ssh_psql(sql.replace('"', '\\"'))
    rows: list[dict] = []
    for line in out.splitlines():
        line = line.strip()
        if not line:
            continue
        try:
            rows.append(json.loads(line))
        except json.JSONDecodeError:
            pass
    return rows


def parse_other(other: str | None) -> dict:
    if not other:
        return {}
    try:
        return json.loads(other)
    except json.JSONDecodeError:
        return {}


def newapi_metrics(rows: list[dict]) -> dict:
    type_counts: dict[int, int] = {}
    ok = [r for r in rows if int(r.get("type") or 0) == 2]
    for r in rows:
        t = int(r.get("type") or 0)
        type_counts[t] = type_counts.get(t, 0) + 1

    read = prompt = 0
    ratio_sum = 0.0
    ratio_n = 0
    for r in ok:
        pt = int(r.get("prompt_tokens") or 0)
        prompt += pt
        o = parse_other(r.get("other"))
        ct = int(o.get("cache_tokens") or 0)
        cr = int(
            o.get("cache_read_input_tokens")
            or o.get("prompt_cache_hit_tokens")
            or o.get("cached_tokens")
            or ct
            or 0
        )
        read += cr
        if o.get("cache_ratio") is not None:
            ratio_sum += float(o["cache_ratio"])
            ratio_n += 1

    return {
        "window_start_cst": SINCE.isoformat(),
        "start_epoch_s": START_S,
        "total_rows": len(rows),
        "type_breakdown": type_counts,
        "ok_rows": len(ok),
        "R1_cache_tokens_over_prompt_pct": round(read / prompt * 100, 2) if prompt else 0,
        "sum_cache_tokens": read,
        "sum_prompt_tokens": prompt,
        "avg_cache_ratio_field": round(ratio_sum / ratio_n, 4) if ratio_n else None,
        "cache_ratio_samples": ratio_n,
    }


def fetch_audit() -> list[dict]:
    proc = subprocess.run(
        [
            "ssh",
            "-o",
            "BatchMode=yes",
            "panda",
            "cat /var/log/zen-proxy-rs/audit/requests-2026-07-03.jsonl",
        ],
        capture_output=True,
        text=True,
        check=False,
    )
    rows = []
    for line in proc.stdout.splitlines():
        try:
            r = json.loads(line)
        except json.JSONDecodeError:
            continue
        if int(r.get("ts") or 0) < START_MS:
            continue
        if MODEL not in str(r.get("public_model") or r.get("model") or ""):
            continue
        rows.append(r)
    return rows


def audit_metrics(rows: list[dict]) -> dict:
    read = miss = prompt = 0
    aff_hit = aff_elig = 0
    pin_hit = pin_elig = 0
    usk_n = 0
    obs: dict[str, int] = {}
    for r in rows:
        read += int(r.get("cache_read_input_tokens") or r.get("cached_tokens") or 0)
        miss += int(r.get("cache_miss_input_tokens") or 0)
        prompt += int(r.get("prompt_tokens") or 0)
        if r.get("usk"):
            usk_n += 1
        if r.get("affinity_key"):
            aff_elig += 1
            aff_hit += int(bool(r.get("affinity_hit")))
        if r.get("session_id"):
            pin_elig += 1
            pin_hit += int(bool(r.get("session_pin_hit")))
        k = str(r.get("provider_cache_observation") or "missing")
        obs[k] = obs.get(k, 0) + 1
    if miss == 0 and prompt > read:
        miss = prompt - read
    return {
        "rows": len(rows),
        "usk_rows": usk_n,
        "usk_pct": round(usk_n / len(rows) * 100, 1) if rows else 0,
        "R1_pct": round(read / prompt * 100, 2) if prompt else 0,
        "R2_pct": round(read / (read + miss) * 100, 2) if read + miss else 0,
        "affinity_hit_pct": round(aff_hit / aff_elig * 100, 2) if aff_elig else 0,
        "pin_hit_pct": round(pin_hit / pin_elig * 100, 2) if pin_elig else 0,
        "provider_obs": obs,
    }


def ccswitch_count() -> dict:
    db = "/root/.cc-switch/cc-switch.db"
    py = f"""
import sqlite3, json
conn=sqlite3.connect('{db}')
cols=[r[1] for r in conn.execute('PRAGMA table_info(proxy_request_logs)')]
print(json.dumps({{'columns': cols}}))
# try ms timestamp
try:
    n=conn.execute("SELECT COUNT(*) FROM proxy_request_logs WHERE started_at >= {START_MS}").fetchone()[0]
    print(json.dumps({{'count_started_at_ms': n}}))
except Exception as e:
    print(json.dumps({{'err_started': str(e)}}))
try:
    rows=conn.execute("SELECT request_model, upstream_model, status_code, COUNT(*) c FROM proxy_request_logs WHERE started_at >= {START_MS} GROUP BY 1,2,3 ORDER BY c DESC LIMIT 10").fetchall()
    print(json.dumps({{'by_model': rows}}))
except Exception as e:
    print(json.dumps({{'err_group': str(e)}}))
"""
    proc = subprocess.run(["python3", "-c", py], capture_output=True, text=True)
    lines = [json.loads(x) for x in proc.stdout.splitlines() if x.strip().startswith("{")]
    out: dict = {}
    for item in lines:
        out.update(item)
    return out


def main() -> None:
    print(f"=== 时间窗 {SINCE.strftime('%Y-%m-%d %H:%M')} CST 至今 | {MODEL} ===\n")

    na_rows = fetch_newapi_rows()
    na = newapi_metrics(na_rows)
    print("## NewAPI (channel 69, Postgres logs.other.cache_tokens)")
    print(json.dumps(na, indent=2, ensure_ascii=False))

    au_rows = fetch_audit()
    au = audit_metrics(au_rows)
    print("\n## ZenProxy audit")
    print(json.dumps(au, indent=2, ensure_ascii=False))

    cs = ccswitch_count()
    print("\n## ccswitch (proxy_request_logs)")
    print(json.dumps(cs, indent=2, ensure_ascii=False))

    print("\n## 口径对照")
    print(
        json.dumps(
            {
                "newapi_R1_cache_tokens": na.get("R1_cache_tokens_over_prompt_pct"),
                "zenproxy_R1": au.get("R1_pct"),
                "zenproxy_R2": au.get("R2_pct"),
                "delta_R1_pp": round(
                    (au.get("R1_pct") or 0) - (na.get("R1_cache_tokens_over_prompt_pct") or 0),
                    2,
                ),
                "newapi_ok_vs_total": f"{na.get('ok_rows')}/{na.get('total_rows')}",
                "usk_coverage_pct": au.get("usk_pct"),
                "affinity_hit_pct": au.get("affinity_hit_pct"),
            },
            indent=2,
        )
    )


if __name__ == "__main__":
    main()
