#!/usr/bin/env python3
"""Generate 10k/50k/100k/200k/350k bucket fixtures for cache matrix acceptance."""

from __future__ import annotations

import json
import os
import sys
from pathlib import Path

_SCRIPT_DIR = Path(__file__).resolve().parent
sys.path.insert(0, str(_SCRIPT_DIR))
from setup_prog_bulk_fixtures import pad_to_chars, rust_skeleton, win_path_to_wsl  # noqa: E402

SPEC = Path(__file__).with_name("TASK_SPEC_CACHE_BUCKET.json")


def main() -> int:
    spec = json.loads(SPEC.read_text(encoding="utf-8"))
    root = Path(os.environ.get("PI_CACHE_BUCKET_ROOT", spec["root"]))
    if ":" in str(root):
        root = win_path_to_wsl(str(root))
    cpt = int(spec.get("chars_per_token_estimate", 4))
    rows = []
    for case in spec["cases"]:
        tokens = int(case["target_context_tokens"])
        chars = tokens * cpt
        case_dir = root / case["dir"]
        header = rust_skeleton(case)
        bulk = pad_to_chars(header, chars)
        src_path = case_dir / case["module"]
        src_path.parent.mkdir(parents=True, exist_ok=True)
        src_path.write_text(bulk, encoding="utf-8")
        meta = {
            "case_id": case["id"],
            "module": case["module"],
            "target_context_tokens": tokens,
            "fixture_bytes": len(bulk.encode("utf-8")),
            "anchors": case.get("anchors", {}),
        }
        (case_dir / "fixture_meta.json").write_text(json.dumps(meta, indent=2) + "\n", encoding="utf-8")
        rows.append(meta)
        print(f"OK {src_path} bytes={meta['fixture_bytes']} target_tokens={tokens}")
    (root / "manifest.json").write_text(
        json.dumps({"root": str(root), "cases": rows}, indent=2) + "\n", encoding="utf-8"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
