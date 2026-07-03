#!/usr/bin/env python3
"""Cache/Quality 99+ 验收脚本 — 解析 zen-proxy audit JSONL。"""

from __future__ import annotations

import argparse
import json
import sys
from collections import defaultdict
from pathlib import Path
from typing import Any


def load_rows(path: Path) -> list[dict[str, Any]]:
    rows: list[dict[str, Any]] = []
    with path.open(encoding="utf-8") as fh:
        for line in fh:
            line = line.strip()
            if not line:
                continue
            try:
                rows.append(json.loads(line))
            except json.JSONDecodeError:
                continue
    return rows


def _usage(row: dict[str, Any]) -> dict[str, Any]:
    if row.get("usage"):
        return row["usage"]
    return row


def r1_pct(rows: list[dict[str, Any]]) -> float:
    """R1 = cache_read / prompt_tokens (Postgres 常见口径)."""
    read = 0
    total = 0
    for row in rows:
        usage = _usage(row)
        prompt = int(usage.get("prompt_tokens") or 0)
        cache_read = int(
            usage.get("cache_read_input_tokens")
            or usage.get("cached_tokens")
            or 0
        )
        if prompt <= 0:
            continue
        read += cache_read
        total += prompt
    return (read / total * 100.0) if total else 0.0


def r2_pct(rows: list[dict[str, Any]]) -> float:
    """R2 = cache_read / (cache_read + cache_miss) — 用户 UI 主口径."""
    read = 0
    miss = 0
    for row in rows:
        usage = _usage(row)
        cache_read = int(
            usage.get("cache_read_input_tokens")
            or usage.get("cached_tokens")
            or 0
        )
        explicit_miss = int(usage.get("cache_miss_input_tokens") or 0)
        if explicit_miss > 0:
            miss += explicit_miss
            read += cache_read
            continue
        prompt = int(usage.get("prompt_tokens") or 0)
        if prompt <= 0 and cache_read <= 0:
            continue
        read += cache_read
        miss += max(prompt - cache_read, 0)
    denom = read + miss
    return (read / denom * 100.0) if denom else 0.0


def r3_pct(rows: list[dict[str, Any]]) -> float:
    """R3 = cache_read / total_tokens (含 output，仅诊断)."""
    read = 0
    total = 0
    for row in rows:
        usage = _usage(row)
        cache_read = int(
            usage.get("cache_read_input_tokens")
            or usage.get("cached_tokens")
            or 0
        )
        tokens = int(usage.get("total_tokens") or 0)
        if tokens <= 0:
            continue
        read += cache_read
        total += tokens
    return (read / total * 100.0) if total else 0.0


def token_weighted_cache(rows: list[dict[str, Any]]) -> float:
    return r2_pct(rows)


def affinity_hit_rate(rows: list[dict[str, Any]]) -> float:
    hits = sum(1 for r in rows if r.get("affinity_hit") is True)
    eligible = sum(1 for r in rows if r.get("affinity_key"))
    return (hits / eligible * 100.0) if eligible else 0.0


def session_pin_hit_rate(rows: list[dict[str, Any]]) -> float:
    hits = sum(1 for r in rows if r.get("session_pin_hit") is True)
    eligible = sum(1 for r in rows if r.get("session_id"))
    return (hits / eligible * 100.0) if eligible else 0.0


def prefix_drift_rate(rows: list[dict[str, Any]]) -> float:
    drift = sum(1 for r in rows if r.get("prefix_drift") is True)
    with_usk = sum(1 for r in rows if r.get("usk"))
    return (drift / with_usk * 100.0) if with_usk else 0.0


def warmup_steady_rate(rows: list[dict[str, Any]]) -> float:
    steady = sum(1 for r in rows if str(r.get("warmup_state") or "") == "steady")
    with_state = sum(1 for r in rows if r.get("warmup_state"))
    return (steady / with_state * 100.0) if with_state else 0.0


def audit_field_coverage(rows: list[dict[str, Any]]) -> dict[str, float]:
    required = [
        "usk",
        "icp_scope",
        "prefix_32k_hash",
        "prompt_cache_key",
        "warmup_state",
        "session_pin_hit",
    ]
    coverage: dict[str, float] = {}
    n = len(rows) or 1
    for field in required:
        present = sum(1 for r in rows if r.get(field) not in (None, "", False) or field == "session_pin_hit")
        if field == "session_pin_hit":
            present = sum(1 for r in rows if "session_pin_hit" in r)
        coverage[field] = present / n * 100.0
    return coverage


def thinking_disabled_count(rows: list[dict[str, Any]]) -> int:
    count = 0
    for row in rows:
        policy = str(row.get("thinking_policy") or "")
        if "disabled" in policy and "probe" not in policy:
            count += 1
    return count


def main() -> int:
    parser = argparse.ArgumentParser(description="Cache/Quality acceptance gate")
    parser.add_argument("audit_jsonl", type=Path)
    parser.add_argument("--min-cache-pct", type=float, default=95.0)
    parser.add_argument("--min-affinity-pct", type=float, default=98.0)
    parser.add_argument("--min-pin-pct", type=float, default=90.0)
    parser.add_argument("--max-prefix-drift-pct", type=float, default=5.0)
    parser.add_argument("--min-audit-coverage-pct", type=float, default=95.0)
    parser.add_argument("--max-thinking-disabled", type=int, default=0)
    parser.add_argument("--strict", action="store_true", help="启用 99+ 严格门槛")
    args = parser.parse_args()

    if not args.audit_jsonl.exists():
        print(f"FAIL: missing {args.audit_jsonl}", file=sys.stderr)
        return 2

    rows = load_rows(args.audit_jsonl)
    if not rows:
        print("FAIL: audit file empty", file=sys.stderr)
        return 2

    by_model: dict[str, list[dict[str, Any]]] = defaultdict(list)
    for row in rows:
        model = str(row.get("public_model") or row.get("model") or "unknown")
        by_model[model].append(row)

    coverage = audit_field_coverage(rows)
    failed = False
    print(f"rows={len(rows)} strict={args.strict}")
    print(
        "audit_coverage: "
        + " ".join(f"{k}={v:.0f}%" for k, v in sorted(coverage.items()))
    )

    min_coverage = 100.0 if args.strict else args.min_audit_coverage_pct
    for field, pct in coverage.items():
        if pct < min_coverage:
            print(f"FAIL audit field {field} coverage={pct:.1f}% < {min_coverage}%")
            failed = True

    for model, model_rows in sorted(by_model.items()):
        r1 = r1_pct(model_rows)
        r2 = r2_pct(model_rows)
        r3 = r3_pct(model_rows)
        aff_pct = affinity_hit_rate(model_rows)
        pin_pct = session_pin_hit_rate(model_rows)
        drift_pct = prefix_drift_rate(model_rows)
        steady_pct = warmup_steady_rate(model_rows)
        disabled = thinking_disabled_count(model_rows)

        is_mimo = "mimo" in model.lower()
        min_cache = 85.0 if is_mimo else args.min_cache_pct
        min_aff = 0.0 if is_mimo else args.min_affinity_pct
        min_pin = 0.0 if is_mimo else args.min_pin_pct

        ok_cache = r2 >= min_cache
        ok_aff = aff_pct >= min_aff or is_mimo
        ok_pin = pin_pct >= min_pin or is_mimo
        ok_drift = drift_pct <= args.max_prefix_drift_pct
        ok_think = disabled <= args.max_thinking_disabled

        if args.strict and not is_mimo:
            ok_cache = r2 >= 95.0
            ok_aff = aff_pct >= 98.0
            ok_pin = pin_pct >= 90.0

        status = "PASS" if ok_cache and ok_aff and ok_pin and ok_drift and ok_think else "FAIL"
        if status == "FAIL":
            failed = True
        print(
            f"{status} model={model} R1={r1:.1f}% R2={r2:.1f}% R3={r3:.1f}% "
            f"affinity={aff_pct:.1f}% pin={pin_pct:.1f}% drift={drift_pct:.1f}% "
            f"steady={steady_pct:.1f}% thinking_disabled={disabled}"
        )

    return 1 if failed else 0


if __name__ == "__main__":
    raise SystemExit(main())
