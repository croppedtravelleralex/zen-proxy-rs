#!/usr/bin/env python3
"""Single-case short_ping smoke: warmup + probe rounds."""
from __future__ import annotations

import json
import sys
from pathlib import Path

_SCRIPT_DIR = Path(__file__).resolve().parent
sys.path.insert(0, str(_SCRIPT_DIR))

from run_pi_bulk_quality_gate import run_pi_session_turn  # noqa: E402
from run_pi_daily_session_cache_test import pi_cache_pct  # noqa: E402

run_dir = Path(r"C:\Users\Lenovo\pi-runs\pi-short-ping-smoke-20260804-1000")
run_dir.mkdir(parents=True, exist_ok=True)
session_dir = run_dir / "sessions" / "short_ping"
raw = run_dir / "raw" / "short_ping"
raw.mkdir(parents=True, exist_ok=True)
pi = r"C:\software\nodejs\node_global\pi.cmd"
cwd = r"D:\SelfMadeTool\pi-matrix-20260801\case_01_ping"
sid = "daily-session-short_ping-smoke-v5"
warmup_prompt = "不要工具。只回复一行：MATRIX_CASE_01_OK"
probe_prompt = "基于上文，用一句话确认任务已完成。最后一行只写：SESSION_PROBE_OK"


def turn(name: str, prompt: str, continue_session: bool) -> dict:
    out_path = raw / f"{name}.stdout"
    err_path = raw / f"{name}.stderr"
    exit_code, timed_out, stdout, stderr, elapsed_ms = run_pi_session_turn(
        prompt=prompt,
        pi_bin=pi,
        thinking="high",
        cwd=cwd,
        session_id=sid,
        session_dir=session_dir,
        timeout_s=300,
        continue_session=continue_session,
        out_path=out_path,
        err_path=err_path,
    )
    usage = pi_cache_pct(stdout, stderr)
    row = {
        "turn": name,
        "exit_code": exit_code,
        "timed_out": timed_out,
        "elapsed_ms": elapsed_ms,
        **usage,
    }
    print(json.dumps(row, ensure_ascii=False))
    return row


warmup = turn("warmup", warmup_prompt, False)
probes: list[dict] = []
for i in range(5):
    probes.append(turn(f"probe-{i:04d}", probe_prompt, True))

pcts = [p.get("pi_cache_pct") for p in probes if p.get("pi_cache_pct") is not None]
summary = {
    "warmup_pct": warmup.get("pi_cache_pct"),
    "probe_0_pct": probes[0].get("pi_cache_pct") if probes else None,
    "probe_0_cacheRead": probes[0].get("cacheRead") if probes else None,
    "probe_0_input": probes[0].get("input") if probes else None,
    "probe_pcts": pcts,
    "probe_min": min(pcts) if pcts else None,
    "probe_p50": sorted(pcts)[len(pcts) // 2] if pcts else None,
    "gate_99": all(p >= 99.0 for p in pcts) if pcts else False,
}
(run_dir / "summary.json").write_text(
    json.dumps(summary, indent=2, ensure_ascii=False) + "\n", encoding="utf-8"
)
print(json.dumps({"event": "summary", **summary}, ensure_ascii=False))
