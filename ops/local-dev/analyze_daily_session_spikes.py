#!/usr/bin/env python3
"""Summarize 0% cache spikes from daily session events.jsonl."""
from __future__ import annotations

import argparse
import json
from pathlib import Path


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("events", type=Path)
    args = parser.parse_args()
    spikes: list[dict] = []
    for line in args.events.read_text(encoding="utf-8").splitlines():
        if not line.strip():
            continue
        row = json.loads(line)
        if row.get("event") != "probe_done":
            continue
        pct = row.get("pi_cache_pct")
        inp = int(row.get("input") or 0)
        cache = int(row.get("cacheRead") or 0)
        if pct is not None and pct <= 1.0 and inp >= 15000:
            spikes.append(
                {
                    "case_id": row.get("case_id"),
                    "probe_index": row.get("probe_index"),
                    "input": inp,
                    "cacheRead": cache,
                    "context_total": row.get("context_total"),
                }
            )
    print(json.dumps({"spike_count": len(spikes), "spikes": spikes}, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
