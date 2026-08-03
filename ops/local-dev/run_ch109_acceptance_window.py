#!/usr/bin/env python3
"""NewAPI ch109 + zen-proxy audit acceptance for a time window.

Primary gate for cache/TTFB validation (not 5-minute Pi alone).
"""

from __future__ import annotations

import argparse
import json
import math
import os
import subprocess
import sys
from pathlib import Path
from typing import Any

BASELINE_REL = Path("docs/baselines/ch109-fix3-pi.json")
TIER_PHASES_REL = Path("docs/baselines/ch109-cache-tier-phases.json")


def find_repo_root(start: Path) -> Path | None:
    for base in [start, *start.parents]:
        if (base / BASELINE_REL).is_file():
            return base
    env_root = os.environ.get("ZEN_SUITE_ROOT")
    if env_root and (Path(env_root) / BASELINE_REL).is_file():
        return Path(env_root)
    wsl_root = Path("//wsl.localhost/HermesUbuntu/home/lenovo/zen-free-model-suite")
    if (wsl_root / BASELINE_REL).is_file():
        return wsl_root
    local_baseline = Path(__file__).resolve().parent / "docs/baselines/ch109-fix3-pi.json"
    if local_baseline.is_file():
        return local_baseline.parent.parent.parent
    return None


_REPO = find_repo_root(Path(__file__).resolve().parent)
ROOT = _REPO if _REPO is not None else Path(__file__).resolve().parents[2]
DEFAULT_BASELINE = ROOT / BASELINE_REL


def percentile(values: list[float], p: float) -> float | None:
    if not values:
        return None
    s = sorted(values)
    i = round((p / 100.0) * (len(s) - 1))
    return s[max(0, min(i, len(s) - 1))]


def ssh_panda_sql(sql: str) -> str:
    cmd = [
        "ssh",
        "-o",
        "BatchMode=yes",
        "-o",
        "ConnectTimeout=20",
        "panda",
        "docker exec new-api-postgres psql -U newapi -d new-api -At -F'	' -c "
        + repr(sql.replace("\n", " ")),
    ]
    proc = subprocess.run(cmd, capture_output=True, text=True, timeout=120)
    if proc.returncode != 0:
        raise RuntimeError(proc.stderr or proc.stdout or "psql failed")
    return proc.stdout


def fetch_newapi_rows(channel_id: int, since_epoch: int, until_epoch: int) -> list[dict[str, Any]]:
    sql = (
        f"SELECT created_at, use_time, prompt_tokens, completion_tokens, type, other::text "
        f"FROM logs WHERE channel_id={channel_id} "
        f"AND created_at >= {since_epoch} AND created_at <= {until_epoch} "
        "ORDER BY created_at"
    )
    raw = ssh_panda_sql(sql)
    rows: list[dict[str, Any]] = []
    for line in raw.splitlines():
        parts = line.split("\t")
        if len(parts) < 6:
            continue
        created_at, use_time, prompt_tokens, completion_tokens, typ, other_raw = parts[:6]
        other: dict[str, Any] = {}
        try:
            other = json.loads(other_raw)
        except json.JSONDecodeError:
            pass
        rows.append(
            {
                "created_at": int(created_at),
                "use_time": int(use_time or 0),
                "prompt_tokens": int(prompt_tokens or 0),
                "completion_tokens": int(completion_tokens or 0),
                "type": int(typ or 0),
                "other": other,
            }
        )
    return rows


def summarize_newapi(rows: list[dict[str, Any]]) -> dict[str, Any]:
    n = len(rows)
    type5 = sum(1 for r in rows if r["type"] == 5)
    prompt_sum = sum(r["prompt_tokens"] for r in rows)
    cache_sum = 0
    frt_vals: list[float] = []
    stream_anomalies = 0
    for r in rows:
        other = r["other"]
        cache_sum += int(other.get("cache_tokens") or 0)
        frt = other.get("frt")
        if frt is not None and int(frt) > 0:
            frt_vals.append(float(frt))
        ss = other.get("stream_status") or {}
        if isinstance(ss, dict):
            status = ss.get("status")
            end_reason = ss.get("end_reason")
            if status != "ok" or end_reason != "eof":
                stream_anomalies += 1
    cache_pct = (100.0 * cache_sum / prompt_sum) if prompt_sum > 0 else 0.0
    return {
        "n": n,
        "type5": type5,
        "prompt_sum": prompt_sum,
        "cache_sum": cache_sum,
        "cache_pct_token_weighted": round(cache_pct, 2),
        "frt_ms": {
            "p50": percentile(frt_vals, 50),
            "p95": percentile(frt_vals, 95),
            "p99": percentile(frt_vals, 99),
            "samples": len(frt_vals),
        },
        "stream_anomalies": stream_anomalies,
    }


def summarize_newapi_tiers(
    rows: list[dict[str, Any]],
    tier_defs: list[dict[str, Any]],
) -> list[dict[str, Any]]:
    """Token-weighted cache% per prompt_tokens band."""
    out: list[dict[str, Any]] = []
    for tier in tier_defs:
        lo = int(tier.get("prompt_tokens_min", 0))
        hi = int(tier.get("prompt_tokens_max_exclusive", 0))
        band_rows = [
            r
            for r in rows
            if lo <= r["prompt_tokens"] < hi and r["type"] == 2
        ]
        prompt_sum = sum(r["prompt_tokens"] for r in band_rows)
        cache_sum = sum(int(r["other"].get("cache_tokens") or 0) for r in band_rows)
        cache_pct = (100.0 * cache_sum / prompt_sum) if prompt_sum > 0 else 0.0
        out.append(
            {
                "id": tier["id"],
                "label": tier.get("label", tier["id"]),
                "prompt_tokens_min": lo,
                "prompt_tokens_max_exclusive": hi,
                "n": len(band_rows),
                "prompt_sum": prompt_sum,
                "cache_sum": cache_sum,
                "cache_pct_token_weighted": round(cache_pct, 2),
            }
        )
    return out


def load_tier_phases(path: Path) -> dict[str, Any]:
    return json.loads(path.read_text(encoding="utf-8"))


def gate_check_tiers(
    tier_summaries: list[dict[str, Any]],
    phases: dict[str, Any],
    phase_id: str,
) -> dict[str, Any]:
    min_pct = float(phases.get("min_cache_pct", 90.0))
    min_samples = int(phases.get("min_ch109_samples_per_tier", 3))
    phase = next((p for p in phases.get("phases", []) if p["id"] == phase_id), None)
    if phase is None:
        return {"pass": False, "error": f"unknown phase {phase_id}", "checks": []}
    enforce = set(phase.get("enforce_tiers", []))
    by_id = {t["id"]: t for t in tier_summaries}
    checks: list[dict[str, Any]] = []
    for tier_id in sorted(enforce):
        row = by_id.get(tier_id, {"n": 0, "cache_pct_token_weighted": 0.0})
        n = int(row.get("n") or 0)
        pct = float(row.get("cache_pct_token_weighted") or 0.0)
        samples_ok = n >= min_samples
        pct_ok = pct >= min_pct if samples_ok else False
        checks.append(
            {
                "name": f"cache_pct_{tier_id}",
                "tier_id": tier_id,
                "value": pct,
                "min": min_pct,
                "samples": n,
                "min_samples": min_samples,
                "pass": samples_ok and pct_ok,
                "reason": (
                    None
                    if samples_ok and pct_ok
                    else (
                        f"samples {n} < {min_samples}"
                        if not samples_ok
                        else f"cache {pct}% < {min_pct}%"
                    )
                ),
            }
        )
    return {
        "phase_id": phase_id,
        "phase_label": phase.get("label"),
        "enforce_tiers": list(enforce),
        "pass": all(c["pass"] for c in checks),
        "checks": checks,
    }


def fetch_audit_summary(since_ms: int, until_ms: int) -> dict[str, Any]:
    cmd = [
        "ssh",
        "-o",
        "BatchMode=yes",
        "panda",
        f"python3 - <<'PY'\n"
        f"import json, os\n"
        f"from collections import Counter\n"
        f"START, END = {since_ms}, {until_ms}\n"
        f"path = '/var/log/zen-proxy-rs/audit/requests-2026-08-01.jsonl'\n"
        f"if not os.path.exists(path):\n"
        f"    print(json.dumps({{'ok': False, 'error': 'no audit file'}}))\n"
        f"    raise SystemExit(0)\n"
        f"c_outcome = Counter()\n"
        f"c_eo_class = Counter()\n"
        f"n = 0\n"
        f"for line in open(path, errors='replace'):\n"
        f"    try: r = json.loads(line)\n"
        f"    except: continue\n"
        f"    ts = r.get('ts') or 0\n"
        f"    if ts < START or ts > END: continue\n"
        f"    n += 1\n"
        f"    c_outcome[r.get('outcome','')]+=1\n"
        f"    c_eo_class[r.get('empty_output_class','') or '']+=1\n"
        f"print(json.dumps({{'ok': True, 'n': n, 'outcome': dict(c_outcome), 'empty_output_class': dict(c_eo_class)}}))\n"
        "PY",
    ]
    proc = subprocess.run(cmd, capture_output=True, text=True, timeout=120)
    if proc.returncode != 0:
        return {"ok": False, "error": (proc.stderr or "")[:500]}
    line = (proc.stdout or "").strip().splitlines()[-1]
    return json.loads(line)


def gate_check(summary: dict[str, Any], baseline: dict[str, Any]) -> dict[str, Any]:
    checks: list[dict[str, Any]] = []
    b_cache = baseline.get("cache_pct_token_weighted", 0)
    b_frt = (baseline.get("frt_ms") or {}).get("p50") or 0
    cache = summary.get("cache_pct_token_weighted", 0)
    frt_p50 = (summary.get("frt_ms") or {}).get("p50")
    checks.append(
        {
            "name": "cache_pct",
            "value": cache,
            "baseline": b_cache,
            "pass": cache >= b_cache - 0.5,
        }
    )
    if frt_p50 is not None:
        checks.append(
            {
                "name": "frt_p50_ms",
                "value": frt_p50,
                "baseline": b_frt,
                "pass": frt_p50 <= b_frt + 500,
            }
        )
    checks.append(
        {
            "name": "type5",
            "value": summary.get("type5", 0),
            "baseline": 0,
            "pass": summary.get("type5", 0) == 0,
        }
    )
    checks.append(
        {
            "name": "stream_anomalies",
            "value": summary.get("stream_anomalies", 0),
            "baseline": baseline.get("stream_anomalies", 0),
            "pass": summary.get("stream_anomalies", 0) <= baseline.get("stream_anomalies", 0),
        }
    )
    return {"pass": all(c["pass"] for c in checks), "checks": checks}


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--channel-id", type=int, default=109)
    parser.add_argument("--since-epoch", type=int, required=True)
    parser.add_argument("--until-epoch", type=int, required=True)
    parser.add_argument("--label", default="window")
    parser.add_argument("--baseline-file", type=Path, default=DEFAULT_BASELINE)
    parser.add_argument(
        "--tier-phases-file",
        type=Path,
        default=ROOT / TIER_PHASES_REL,
        help="Phased cache tier definitions (docs/baselines/ch109-cache-tier-phases.json)",
    )
    parser.add_argument(
        "--phase",
        default="",
        help="If set, gate enforced tiers for this phase (e.g. phase_1) at min_cache_pct",
    )
    parser.add_argument("--out", type=Path, default="")
    args = parser.parse_args(argv)

    baseline = json.loads(args.baseline_file.read_text(encoding="utf-8"))
    rows = fetch_newapi_rows(args.channel_id, args.since_epoch, args.until_epoch)
    summary = summarize_newapi(rows)
    tier_phases: dict[str, Any] | None = None
    tier_summaries: list[dict[str, Any]] | None = None
    tier_gate: dict[str, Any] | None = None
    if args.tier_phases_file.is_file():
        tier_phases = load_tier_phases(args.tier_phases_file)
        tier_summaries = summarize_newapi_tiers(rows, tier_phases.get("tiers", []))
        summary["tiers"] = tier_summaries
    since_ms = args.since_epoch * 1000
    until_ms = args.until_epoch * 1000
    audit = fetch_audit_summary(since_ms, until_ms)
    gate = gate_check(summary, baseline)
    if args.phase and tier_phases and tier_summaries is not None:
        tier_gate = gate_check_tiers(tier_summaries, tier_phases, args.phase)
        gate["tier_phase"] = tier_gate
        gate["pass"] = gate["pass"] and tier_gate["pass"]

    report = {
        "label": args.label,
        "channel_id": args.channel_id,
        "since_epoch": args.since_epoch,
        "until_epoch": args.until_epoch,
        "newapi": summary,
        "audit": audit,
        "gate": gate,
        "baseline_label": baseline.get("label"),
        "tier_phases_label": (tier_phases or {}).get("label"),
        "phase": args.phase or None,
    }

    text = json.dumps(report, indent=2, ensure_ascii=False)
    print(text)
    if args.out:
        out = Path(args.out)
        out.parent.mkdir(parents=True, exist_ok=True)
        out.write_text(text + "\n", encoding="utf-8")
    return 0 if gate["pass"] else 1


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
