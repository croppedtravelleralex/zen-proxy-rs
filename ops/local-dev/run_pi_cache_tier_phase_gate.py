#!/usr/bin/env python3
"""Phased cache tier gate: warmup + multi-turn probe per band, 90%+ per phase.

Phases (docs/baselines/ch109-cache-tier-phases.json):
  phase_1: prompt < 50k
  phase_2: + 50k–100k
  phase_3: + 100k–200k
  phase_4: + 200k–300k

Each phase runs HTTP probes (closeTest → ch109 → zen-proxy-test) for new tiers,
re-validates prior tiers, and gates ch109 token-weighted cache% per band.

Does not touch production zen-proxy 4001/4002/4004.
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
import time
from pathlib import Path
from typing import Any

_SCRIPT_DIR = Path(__file__).resolve().parent
sys.path.insert(0, str(_SCRIPT_DIR))

from run_cache_bucket_single_probe import (  # noqa: E402
    GATE_METRIC,
    run_tier_probe,
    SPEC_DEFAULT,
)
from run_prog_bulk_context_gate import load_key, resolve_spec_root  # noqa: E402

TIER_PHASES_DEFAULT = Path(__file__).resolve().parents[2] / "docs/baselines/ch109-cache-tier-phases.json"


def load_tier_phases(path: Path) -> dict[str, Any]:
    return json.loads(path.read_text(encoding="utf-8"))


def resolve_phase(phases_doc: dict[str, Any], phase_id: str) -> dict[str, Any]:
    for row in phases_doc.get("phases", []):
        if row["id"] == phase_id:
            return row
    raise SystemExit(f"unknown phase: {phase_id}")


def case_ids_for_phase(phases_doc: dict[str, Any], phase_id: str) -> list[str]:
    phase = resolve_phase(phases_doc, phase_id)
    enforce = phase.get("enforce_tiers", [])
    tier_by_id = {t["id"]: t for t in phases_doc.get("tiers", [])}
    ids: list[str] = []
    for tier_id in enforce:
        tier = tier_by_id.get(tier_id)
        if not tier:
            raise SystemExit(f"phase {phase_id} references unknown tier {tier_id}")
        ids.append(str(tier["probe_case_id"]))
    return ids


def run_ch109_tier_acceptance(
    run_dir: Path,
    since_epoch: int,
    until_epoch: int,
    label: str,
    phase_id: str,
    tier_phases_file: Path,
) -> dict[str, Any] | None:
    script = Path(__file__).resolve().parent / "run_ch109_acceptance_window.py"
    out = run_dir / "ch109_acceptance.json"
    cmd = [
        sys.executable,
        str(script),
        "--since-epoch",
        str(since_epoch),
        "--until-epoch",
        str(until_epoch),
        "--label",
        label,
        "--phase",
        phase_id,
        "--tier-phases-file",
        str(tier_phases_file),
        "--out",
        str(out),
    ]
    print(json.dumps({"event": "ch109_tier_acceptance_start", "cmd": cmd}, ensure_ascii=False))
    proc = subprocess.run(cmd, capture_output=True, text=True, encoding="utf-8", errors="replace")
    (run_dir / "ch109_acceptance.stdout").write_text(proc.stdout, encoding="utf-8")
    (run_dir / "ch109_acceptance.stderr").write_text(proc.stderr, encoding="utf-8")
    if out.exists():
        return json.loads(out.read_text(encoding="utf-8"))
    return None


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--phase", default="phase_1", help="phase_1 … phase_4")
    parser.add_argument("--tier-phases-file", type=Path, default=TIER_PHASES_DEFAULT)
    parser.add_argument("--spec", default=str(SPEC_DEFAULT))
    parser.add_argument("--base-url", default="https://sub2api.closeapi.top")
    parser.add_argument("--model", default="deepseek-v4-flash")
    parser.add_argument("--max-tokens", type=int, default=128)
    parser.add_argument("--timeout-s", type=int, default=600)
    parser.add_argument("--client-header", default="claude-code")
    parser.add_argument("--run-dir", default="")
    parser.add_argument(
        "--min-newapi-cache-pct",
        type=float,
        default=None,
        help="Per-probe gate; default from tier-phases file (90)",
    )
    args = parser.parse_args(argv)

    if not args.tier_phases_file.is_file():
        print(f"tier phases file missing: {args.tier_phases_file}", file=sys.stderr)
        return 2

    phases_doc = load_tier_phases(args.tier_phases_file)
    phase_row = resolve_phase(phases_doc, args.phase)
    case_ids = case_ids_for_phase(phases_doc, args.phase)
    min_pct = float(
        args.min_newapi_cache_pct
        if args.min_newapi_cache_pct is not None
        else phases_doc.get("min_newapi_cache_pct_probe", 90.0)
    )

    spec = json.loads(Path(args.spec).read_text(encoding="utf-8"))
    root = resolve_spec_root(spec)
    if not root.exists():
        print("run: python3 ops/local-dev/pi-matrix/setup_cache_bucket_fixtures.py", file=sys.stderr)
        return 2

    cases_by_id = {c["id"]: c for c in spec["cases"]}
    missing = [cid for cid in case_ids if cid not in cases_by_id]
    if missing:
        print(f"spec missing cases: {missing}", file=sys.stderr)
        return 2

    key = load_key()
    stamp = time.strftime("%Y%m%d-%H%M%S")
    run_dir = (
        Path(args.run_dir)
        if args.run_dir
        else Path(__file__).resolve().parents[2] / ".local-dev" / "runs" / f"cache-tier-{args.phase}-{stamp}"
    )
    run_dir.mkdir(parents=True, exist_ok=True)

    meta = {
        "phase": args.phase,
        "phase_label": phase_row.get("label"),
        "enforce_tiers": phase_row.get("enforce_tiers"),
        "case_ids": case_ids,
        "min_newapi_cache_pct": min_pct,
        "gate_metric": GATE_METRIC,
        "route": "closeTest NewAPI -> ch109 -> panda :4010 -> zen-proxy-test :4011",
    }
    (run_dir / "phase_meta.json").write_text(json.dumps(meta, indent=2) + "\n", encoding="utf-8")
    print(json.dumps({"event": "start", "run_dir": str(run_dir), **meta}, ensure_ascii=False))

    wall_start = int(time.time())
    t0 = time.monotonic()
    tiers_out = [
        run_tier_probe(
            cases_by_id[cid],
            root,
            args.base_url,
            key,
            args.model,
            args.timeout_s,
            run_dir,
            args.client_header,
            args.max_tokens,
            spec.get("min_prompt_tokens", {}),
            min_pct,
        )
        for cid in case_ids
    ]
    wall_ms = int((time.monotonic() - t0) * 1000)
    wall_end = int(time.time())

    probe_gate_pass = all(t["pass"] for t in tiers_out)
    ch109 = run_ch109_tier_acceptance(
        run_dir,
        wall_start,
        wall_end,
        f"cache-tier-{args.phase}",
        args.phase,
        args.tier_phases_file,
    )
    ch109_tier_pass = True
    if ch109 and ch109.get("gate", {}).get("tier_phase"):
        ch109_tier_pass = bool(ch109["gate"]["tier_phase"].get("pass"))

    final = {
        "event": "done",
        "run_dir": str(run_dir),
        "phase": args.phase,
        "wall_ms": wall_ms,
        "probe_gate_pass": probe_gate_pass,
        "ch109_tier_gate_pass": ch109_tier_pass,
        "gate_pass": probe_gate_pass and ch109_tier_pass,
        "min_newapi_cache_pct": min_pct,
        "tiers": [
            {
                "case_id": t["case_id"],
                "tier": t["tier"],
                "probe_newapi_cache_pct": t["probe"]["newapi_cache_pct"],
                "probe_prompt_tokens": t["probe"]["prompt_tokens"],
                "pass": t["pass"],
                "issues": t.get("issues"),
            }
            for t in tiers_out
        ],
        "ch109_tiers": (ch109 or {}).get("newapi", {}).get("tiers"),
        "ch109_tier_phase": (ch109 or {}).get("gate", {}).get("tier_phase"),
    }
    (run_dir / "final.json").write_text(json.dumps(final, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(final, ensure_ascii=False))
    return 0 if final["gate_pass"] else 1


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
