#!/usr/bin/env python3
"""三方缓存对账：ZenProxy audit + NewAPI Postgres + ccswitch SQLite（可选）。"""
from __future__ import annotations

import json
import sqlite3
import subprocess
import sys
from collections import Counter, defaultdict
from datetime import datetime, timedelta, timezone
from pathlib import Path
from typing import Any

CST = timezone(timedelta(hours=8))


def parse_args() -> Any:
    import argparse

    p = argparse.ArgumentParser()
    p.add_argument("--since", default="2026-07-03 13:57:00", help="CST 起始时间")
    p.add_argument("--model", default="deepseek-v4-flash")
    p.add_argument("--audit", type=Path, default=None)
    p.add_argument("--ccswitch-db", type=Path, default=None)
    p.add_argument("--remote-audit", action="store_true")
    return p.parse_args()


def since_ms(since: str) -> int:
    dt = datetime.strptime(since, "%Y-%m-%d %H:%M:%S").replace(tzinfo=CST)
    return int(dt.timestamp() * 1000)


def since_epoch_s(since: str) -> int:
    dt = datetime.strptime(since, "%Y-%m-%d %H:%M:%S").replace(tzinfo=CST)
    return int(dt.timestamp())


def r1(rows: list[dict], read_k: str, prompt_k: str) -> tuple[int, int, float]:
    read = prompt = 0
    for r in rows:
        read += int(r.get(read_k) or 0)
        prompt += int(r.get(prompt_k) or 0)
    pct = (read / prompt * 100) if prompt else 0.0
    return read, prompt, pct


def r2_from_usage(read: int, miss: int) -> float:
    d = read + miss
    return (read / d * 100) if d else 0.0


def load_audit(path: Path, start_ms: int, model: str) -> list[dict]:
    out: list[dict] = []
    with path.open(encoding="utf-8") as fh:
        for line in fh:
            line = line.strip()
            if not line:
                continue
            try:
                r = json.loads(line)
            except json.JSONDecodeError:
                continue
            ts = int(r.get("ts") or 0)
            if ts < start_ms:
                continue
            m = str(r.get("public_model") or r.get("model") or "")
            if model not in m and m != model:
                continue
            out.append(r)
    return out


def fetch_audit_remote(start_ms: int, model: str) -> list[dict]:
    day = datetime.fromtimestamp(start_ms / 1000, tz=CST).strftime("%Y-%m-%d")
    remote = f"/var/log/zen-proxy-rs/audit/requests-{day}.jsonl"
    proc = subprocess.run(
        ["ssh", "-o", "BatchMode=yes", "panda", f"cat {remote}"],
        capture_output=True,
        text=True,
        check=False,
    )
    if proc.returncode != 0:
        print(f"WARN: remote audit fetch failed: {proc.stderr}", file=sys.stderr)
        return []
    out: list[dict] = []
    for line in proc.stdout.splitlines():
        if not line.strip():
            continue
        try:
            r = json.loads(line)
        except json.JSONDecodeError:
            continue
        ts = int(r.get("ts") or 0)
        if ts < start_ms:
            continue
        m = str(r.get("public_model") or r.get("model") or "")
        if model not in m and m != model:
            continue
        out.append(r)
    return out


def zenproxy_stats(rows: list[dict]) -> dict[str, Any]:
    if not rows:
        return {"rows": 0}

    read = miss = prompt = creation = 0
    aff_hit = aff_elig = pin_hit = pin_elig = 0
    usk_present = 0
    diag: Counter[str] = Counter()
    obs: Counter[str] = Counter()

    for r in rows:
        read += int(r.get("cache_read_input_tokens") or r.get("cached_tokens") or 0)
        miss += int(r.get("cache_miss_input_tokens") or 0)
        prompt += int(r.get("prompt_tokens") or 0)
        creation += int(r.get("cache_creation_input_tokens") or 0)
        if r.get("affinity_key"):
            aff_elig += 1
            if r.get("affinity_hit"):
                aff_hit += 1
        if r.get("session_id"):
            pin_elig += 1
            if r.get("session_pin_hit"):
                pin_hit += 1
        if r.get("usk"):
            usk_present += 1
        obs[str(r.get("provider_cache_observation") or "missing")] += 1
        if r.get("prefix_drift"):
            diag["prefix_drift"] += 1
        elif r.get("affinity_key") and not r.get("affinity_hit"):
            diag["affinity_miss"] += 1
        elif r.get("session_id") and not r.get("session_pin_hit"):
            diag["pin_miss"] += 1
        else:
            diag["other"] += 1

    if miss == 0 and prompt > read:
        miss = prompt - read

    return {
        "rows": len(rows),
        "R1_pct": round(read / prompt * 100, 2) if prompt else 0,
        "R2_pct": round(r2_from_usage(read, miss), 2),
        "cache_read": read,
        "cache_miss": miss,
        "prompt_tokens": prompt,
        "cache_creation": creation,
        "affinity_hit_pct": round(aff_hit / aff_elig * 100, 2) if aff_elig else 0,
        "affinity_eligible": aff_elig,
        "pin_hit_pct": round(pin_hit / pin_elig * 100, 2) if pin_elig else 0,
        "usk_rows": usk_present,
        "provider_obs": dict(obs),
        "diagnostics": dict(diag),
    }


def fetch_newapi_pg(start_s: int, model: str) -> list[dict]:
    sql = f"""
SELECT json_build_object(
  'request_id', request_id,
  'model_name', model_name,
  'channel_id', channel_id,
  'type', type,
  'created_at', created_at,
  'prompt_tokens', prompt_tokens,
  'completion_tokens', completion_tokens,
  'other', other
)::text
FROM logs
WHERE created_at >= {start_s}
  AND channel_id = 69
  AND model_name ILIKE '%{model}%'
ORDER BY created_at;
"""
    proc = subprocess.run(
        [
            "ssh",
            "-o",
            "BatchMode=yes",
            "panda",
            f"docker exec new-api-postgres psql -U newapi -d new-api -At -c \"{sql.replace(chr(10), ' ')}\"",
        ],
        capture_output=True,
        text=True,
        check=False,
    )
    if proc.returncode != 0:
        return [{"error": proc.stderr.strip() or proc.stdout.strip()}]
    rows: list[dict] = []
    for line in proc.stdout.splitlines():
        line = line.strip()
        if not line:
            continue
        try:
            rows.append(json.loads(line))
        except json.JSONDecodeError:
            rows.append({"raw": line})
    return rows


def parse_newapi_other(other: Any) -> dict[str, int]:
    if other is None:
        return {}
    if isinstance(other, dict):
        data = other
    else:
        try:
            data = json.loads(str(other))
        except json.JSONDecodeError:
            return {}
    out: dict[str, int] = {}
    for k in (
        "cache_read_input_tokens",
        "cached_tokens",
        "prompt_cache_hit_tokens",
        "cache_creation_input_tokens",
        "prompt_cache_miss_tokens",
    ):
        if k in data and data[k] is not None:
            out[k] = int(data[k])
    usage = data.get("usage") if isinstance(data.get("usage"), dict) else {}
    for k in (
        "cache_read_input_tokens",
        "cached_tokens",
        "prompt_cache_hit_tokens",
        "cache_creation_input_tokens",
    ):
        if k in usage and k not in out:
            out[k] = int(usage[k])
    return out


def newapi_stats(rows: list[dict]) -> dict[str, Any]:
    if not rows:
        return {"rows": 0}
    if rows and rows[0].get("error"):
        return {"rows": 0, "error": rows[0]["error"]}

    read = prompt = 0
    with_cache_field = 0
    errors = 0
    for r in rows:
        if int(r.get("type") or 0) != 2:
            errors += 1
            continue
        pt = int(r.get("prompt_tokens") or 0)
        prompt += pt
        other = parse_newapi_other(r.get("other"))
        cr = (
            other.get("cache_read_input_tokens")
            or other.get("cached_tokens")
            or other.get("prompt_cache_hit_tokens")
            or 0
        )
        if cr or other:
            with_cache_field += 1
        read += cr

    return {
        "rows": len(rows),
        "ok_rows": len(rows) - errors,
        "errors": errors,
        "R1_pct": round(read / prompt * 100, 2) if prompt else 0,
        "cache_read": read,
        "prompt_tokens": prompt,
        "rows_with_cache_in_other": with_cache_field,
    }


def find_ccswitch_db() -> Path | None:
    candidates = [
        Path("/mnt/c/Users/Lenovo/AppData/Roaming/cc-switch/cc-switch.db"),
        Path.home() / ".cc-switch" / "cc-switch.db",
        Path("/tmp/cc-switch.db"),
    ]
    for p in candidates:
        if p.exists():
            return p
    return None


def ccswitch_stats(db: Path, start_ms: int, model: str) -> dict[str, Any]:
    conn = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
    conn.row_factory = sqlite3.Row
    tables = [r[0] for r in conn.execute("SELECT name FROM sqlite_master WHERE type='table'")]
    # 常见表名探测
    for table in ("requests", "usage_logs", "logs", "api_logs", "history"):
        if table not in tables:
            continue
        cols = [r[1] for r in conn.execute(f"PRAGMA table_info({table})")]
        colset = set(cols)
        time_col = next(
            (c for c in ("created_at", "timestamp", "time", "started_at") if c in colset),
            None,
        )
        model_col = next(
            (c for c in ("model", "request_model", "model_name") if c in colset),
            None,
        )
        if not time_col:
            continue
        q = f"SELECT * FROM {table} WHERE {time_col} >= ?"
        params: list[Any] = [start_ms if "ms" in time_col or start_ms > 10_000_000_000 else start_ms // 1000]
        if model_col:
            q += f" AND {model_col} LIKE ?"
            params.append(f"%{model}%")
        try:
            fetched = conn.execute(q, params).fetchall()
        except sqlite3.Error:
            continue
        if not fetched:
            continue
        return {
            "rows": len(fetched),
            "table": table,
            "columns": cols,
            "sample_keys": list(dict(fetched[0]).keys())[:15],
            "note": "ccswitch 通常无 provider cache token；仅作请求量/模型对账",
        }
    return {"rows": 0, "tables": tables, "note": "未找到可解析的时间序列表"}


def main() -> int:
    args = parse_args()
    start_ms = since_ms(args.since)
    start_s = since_epoch_s(args.since)

    print(f"=== 窗口：{args.since} CST 至今 | 模型：{args.model} ===\n")

    if args.remote_audit or args.audit is None:
        audit_rows = fetch_audit_remote(start_ms, args.model)
    else:
        audit_rows = load_audit(args.audit, start_ms, args.model)

    zp = zenproxy_stats(audit_rows)
    print("## ZenProxy audit")
    print(json.dumps(zp, indent=2, ensure_ascii=False))

    na_rows = fetch_newapi_pg(start_s, args.model)
    na = newapi_stats(na_rows)
    print("\n## NewAPI Postgres (channel 69)")
    print(json.dumps(na, indent=2, ensure_ascii=False))

    db = args.ccswitch_db or find_ccswitch_db()
    if db:
        cs = ccswitch_stats(db, start_ms, args.model)
        print(f"\n## ccswitch ({db})")
        print(json.dumps(cs, indent=2, ensure_ascii=False))
    else:
        print("\n## ccswitch")
        print(json.dumps({"rows": 0, "note": "未找到本地 SQLite；ccswitch 仅统计请求路由，不含 cache token"}, ensure_ascii=False))

    # join sample
    if audit_rows and na_rows and not na.get("error"):
        ext_ids = {str(r.get("external_request_id") or "") for r in audit_rows if r.get("external_request_id")}
        matched = sum(1 for r in na_rows if str(r.get("request_id") or "") in ext_ids)
        print(f"\n## Join（external_request_id ↔ request_id）")
        print(json.dumps({"audit_rows": len(audit_rows), "newapi_rows": len(na_rows), "matched": matched}, indent=2))

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
