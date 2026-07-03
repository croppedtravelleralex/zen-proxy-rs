#!/usr/bin/env python3
"""四层 join 对账：audit JSONL + 可选 NewAPI logs 导出。"""

from __future__ import annotations

import argparse
import json
import sys
from collections import Counter, defaultdict
from pathlib import Path
from typing import Any


def load_jsonl(path: Path) -> list[dict[str, Any]]:
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


def diagnose_row(row: dict[str, Any]) -> str:
    if row.get("prefix_drift"):
        return "D5_prefix_drift"
    if row.get("affinity_key") and not row.get("affinity_hit"):
        return "D1_affinity_miss"
    if row.get("session_id") and not row.get("session_pin_hit"):
        return "D2_pin_miss"
    obs = str(row.get("provider_cache_observation") or "")
    if obs == "no_cache_signal":
        return "D3_no_provider_cache"
    if obs == "cache_write" and int(row.get("cache_read_input_tokens") or 0) == 0:
        return "D4_warmup_write"
    if obs == "cache_hit":
        return "OK_cache_hit"
    return "OK_unknown"


def summarize_audit(rows: list[dict[str, Any]]) -> dict[str, Any]:
    by_diag: Counter[str] = Counter()
    by_model: dict[str, Counter[str]] = defaultdict(Counter)
    for row in rows:
        diag = diagnose_row(row)
        model = str(row.get("public_model") or row.get("model") or "unknown")
        by_diag[diag] += 1
        by_model[model][diag] += 1
    return {
        "rows": len(rows),
        "diagnostics": dict(by_diag),
        "by_model": {m: dict(c) for m, c in sorted(by_model.items())},
    }


def join_newapi(audit_rows: list[dict[str, Any]], newapi_rows: list[dict[str, Any]]) -> dict[str, Any]:
  """按 external_request_id / rid 粗 join。"""
  audit_by_ext = {
      str(r.get("external_request_id") or r.get("rid") or ""): r
      for r in audit_rows
      if r.get("external_request_id") or r.get("rid")
  }
  matched = 0
  r2_audit = 0.0
  r2_newapi = 0.0
  mismatches = 0
  for nrow in newapi_rows:
      key = str(nrow.get("request_id") or nrow.get("id") or "")
      if not key or key not in audit_by_ext:
          continue
      matched += 1
      arow = audit_by_ext[key]
      a_read = int(arow.get("cache_read_input_tokens") or arow.get("cached_tokens") or 0)
      a_miss = int(arow.get("cache_miss_input_tokens") or 0)
      if a_read + a_miss > 0:
          r2_audit += a_read / (a_read + a_miss)
      n_read = int(nrow.get("cache_read_input_tokens") or nrow.get("cached_tokens") or 0)
      n_prompt = int(nrow.get("prompt_tokens") or 0)
      if n_prompt > 0:
          r2_newapi += n_read / n_prompt
      if n_prompt > 0 and a_miss > 0:
          expected_r1 = n_read / n_prompt
          actual_r2 = a_read / (a_read + a_miss) if a_read + a_miss > 0 else 0
          if abs(expected_r1 - actual_r2) > 0.15:
              mismatches += 1
  if matched:
      r2_audit = r2_audit / matched * 100
      r2_newapi = r2_newapi / matched * 100
  return {
      "matched": matched,
      "r2_audit_avg_pct": round(r2_audit, 2),
      "r1_newapi_avg_pct": round(r2_newapi, 2),
      "r1_r2_mismatch_count": mismatches,
  }


def main() -> int:
    parser = argparse.ArgumentParser(description="Cache join diagnostic report")
    parser.add_argument("audit_jsonl", type=Path)
    parser.add_argument("--newapi-jsonl", type=Path, default=None)
    parser.add_argument("--output", type=Path, default=None)
    args = parser.parse_args()

    if not args.audit_jsonl.exists():
        print(f"FAIL: missing {args.audit_jsonl}", file=sys.stderr)
        return 2

    audit_rows = load_jsonl(args.audit_jsonl)
    report: dict[str, Any] = {"audit": summarize_audit(audit_rows)}

    if args.newapi_jsonl and args.newapi_jsonl.exists():
        newapi_rows = load_jsonl(args.newapi_jsonl)
        report["join"] = join_newapi(audit_rows, newapi_rows)

    text = json.dumps(report, indent=2, ensure_ascii=False)
    if args.output:
        args.output.write_text(text, encoding="utf-8")
    print(text)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
