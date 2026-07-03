#!/usr/bin/env python3
"""抽样检查部署后 audit 是否写入 CCP 字段。"""
from __future__ import annotations

import json
import sys
from pathlib import Path


def main() -> int:
    path = Path(sys.argv[1])
    rows: list[dict] = []
    for line in path.read_text(encoding="utf-8").splitlines():
        line = line.strip()
        if not line:
            continue
        try:
            rows.append(json.loads(line))
        except json.JSONDecodeError:
            continue

    print(f"total_rows={len(rows)}")
    if not rows:
        return 1

    with_usk = [r for r in rows if r.get("usk")]
    print(f"rows_with_usk={len(with_usk)}")

    sample = with_usk[-10:] if with_usk else rows[-10:]
    print("sample:")
    for i, r in enumerate(sample):
        print(
            f"  [{i}] model={r.get('public_model')} "
            f"affinity_hit={r.get('affinity_hit')} "
            f"pin={r.get('session_pin_hit')} "
            f"usk={bool(r.get('usk'))} "
            f"warmup={r.get('warmup_state')} "
            f"ts={r.get('ts')}"
        )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
