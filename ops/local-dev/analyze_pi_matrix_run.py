#!/usr/bin/env python3
"""Comprehensive post-run analysis for pi-matrix load tests."""

from __future__ import annotations

import argparse
import json
import re
import statistics
from collections import Counter, defaultdict
from pathlib import Path
from typing import Any


EMPTY_PATTERNS = [
    re.compile(r"empty_output", re.I),
    re.compile(r"no assistant", re.I),
    re.compile(r"completion=0", re.I),
    re.compile(r"bytes_received=0", re.I),
]
TRUNC_PATTERNS = [
    re.compile(r"truncat", re.I),
    re.compile(r"stream truncated", re.I),
    re.compile(r"max_tokens", re.I),
    re.compile(r"length", re.I),
    re.compile(r"stop_reason.*length", re.I),
]
ERROR_PATTERNS = [
    re.compile(r"502", re.I),
    re.compile(r"503", re.I),
    re.compile(r"rate.?limit", re.I),
    re.compile(r"api error", re.I),
    re.compile(r"retrying", re.I),
]


def load_json(path: Path) -> Any:
    if not path.is_file():
        return None
    return json.loads(path.read_text(encoding="utf-8"))


def parse_pi_stdout(path: Path) -> dict[str, Any]:
    text = path.read_text(encoding="utf-8", errors="replace")
    assistant_chars = 0
    tool_calls = 0
    thinking_chars = 0
    stop_reasons: list[str] = []
    errors: list[str] = []
    json_events = 0
    for line in text.splitlines():
        line = line.strip()
        if not line.startswith("{") or not line.endswith("}"):
            continue
        try:
            obj = json.loads(line)
        except json.JSONDecodeError:
            continue
        json_events += 1
        blob = json.dumps(obj, ensure_ascii=False)
        if any(p.search(blob) for p in EMPTY_PATTERNS):
            errors.append("empty_output_marker")
        if any(p.search(blob) for p in TRUNC_PATTERNS):
            errors.append("truncation_marker")
        if any(p.search(blob) for p in ERROR_PATTERNS):
            errors.append("upstream_error_marker")
        msg = obj.get("message") if isinstance(obj.get("message"), dict) else obj
        if isinstance(msg, dict):
            sr = msg.get("stopReason") or msg.get("rawStopReason")
            if sr:
                stop_reasons.append(str(sr))
            content = msg.get("content")
            if isinstance(content, list):
                for block in content:
                    if not isinstance(block, dict):
                        continue
                    if block.get("type") == "thinking" and isinstance(block.get("thinking"), str):
                        thinking_chars += len(block["thinking"])
                    if block.get("type") == "text" and isinstance(block.get("text"), str):
                        assistant_chars += len(block["text"])
                    if block.get("type") == "toolCall" or block.get("type") == "tool_use":
                        tool_calls += 1
            elif isinstance(content, str):
                assistant_chars += len(content)
    merged_lower = text.lower()
    if assistant_chars == 0 and json_events > 2:
        errors.append("zero_assistant_text")
    if len(text) < 200 and json_events > 0:
        errors.append("suspiciously_short_output")
    return {
        "path": str(path),
        "bytes": len(text.encode("utf-8", errors="replace")),
        "json_events": json_events,
        "assistant_chars": assistant_chars,
        "thinking_chars": thinking_chars,
        "tool_calls": tool_calls,
        "stop_reasons": stop_reasons,
        "issues": list(dict.fromkeys(errors)),
        "has_self_check_pass": "self_check_pass" in merged_lower,
        "has_self_check_fail": "self_check_fail" in merged_lower,
    }


def pct(vals: list[float], p: float) -> float | None:
    if not vals:
        return None
    s = sorted(vals)
    i = round((p / 100.0) * (len(s) - 1))
    return s[max(0, min(i, len(s) - 1))]


def analyze_events(events_path: Path) -> dict[str, Any]:
    rows: list[dict[str, Any]] = []
    if events_path.is_file():
        for line in events_path.read_text(encoding="utf-8", errors="replace").splitlines():
            if line.strip():
                try:
                    rows.append(json.loads(line))
                except json.JSONDecodeError:
                    pass
    elapsed = [float(r.get("elapsed_ms", 0)) for r in rows if r.get("elapsed_ms")]
    by_case: dict[str, list[dict[str, Any]]] = defaultdict(list)
    for r in rows:
        by_case[r.get("case_id", "unknown")].append(r)
    case_stats = []
    for case_id, items in sorted(by_case.items()):
        el = [float(x.get("elapsed_ms", 0)) for x in items if x.get("elapsed_ms")]
        case_stats.append(
            {
                "case_id": case_id,
                "iterations": len(items),
                "timeouts": sum(1 for x in items if x.get("timed_out")),
                "ok_exit": sum(1 for x in items if x.get("exit_code") == 0 and not x.get("timed_out")),
                "semantic_ok": sum(1 for x in items if x.get("semantic_ok")),
                "elapsed_ms": {"p50": pct(el, 50), "p95": pct(el, 95), "max": max(el) if el else None},
                "jitter_p95_p50": (pct(el, 95) - pct(el, 50)) if el and pct(el, 95) and pct(el, 50) else None,
            }
        )
    return {
        "total_events": len(rows),
        "elapsed_ms": {"p50": pct(elapsed, 50), "p90": pct(elapsed, 90), "p95": pct(elapsed, 95), "max": max(elapsed) if elapsed else None},
        "timeout_count": sum(1 for r in rows if r.get("timed_out")),
        "error_markers_sum": sum(int(r.get("error_markers", 0)) for r in rows),
        "cases": case_stats,
    }


def analyze_raw_outputs(run_dir: Path, spec: dict[str, Any]) -> dict[str, Any]:
    raw = run_dir / "raw"
    per_case: list[dict[str, Any]] = []
    empty_like = 0
    trunc_like = 0
    for case in spec.get("cases", []):
        cid = case["id"]
        case_dir = raw / cid
        outputs: list[dict[str, Any]] = []
        if case_dir.is_dir():
            for p in sorted(case_dir.glob("iter-*.stdout")):
                outputs.append(parse_pi_stdout(p))
        gate = raw / f"{cid}.stdout"
        if gate.is_file():
            outputs.append(parse_pi_stdout(gate))
        if not outputs:
            per_case.append({"case_id": cid, "samples": 0, "issues": ["no_stdout"]})
            continue
        best = max(outputs, key=lambda o: o["assistant_chars"])
        issues = list(best.get("issues", []))
        for token in case.get("expected_tokens", []):
            if token.lower() not in gate.read_text(encoding="utf-8", errors="replace").lower() if gate.is_file() else True:
                if gate.is_file():
                    if token.lower() not in gate.read_text(encoding="utf-8", errors="replace").lower():
                        issues.append(f"missing_token:{token}")
        if best["assistant_chars"] < 40:
            empty_like += 1
            issues.append("low_assistant_output")
        if any("truncation" in i for i in best.get("issues", [])):
            trunc_like += 1
        per_case.append(
            {
                "case_id": cid,
                "target_context_tokens": case.get("target_context_tokens"),
                "samples": len(outputs),
                "best": best,
                "issues": list(dict.fromkeys(issues)),
                "quality_pass": len(issues) == 0,
            }
        )
    return {
        "cases": per_case,
        "empty_like_cases": empty_like,
        "truncation_like_cases": trunc_like,
        "quality_pass_count": sum(1 for c in per_case if c.get("quality_pass")),
    }


def analyze_monitor(monitor_path: Path) -> dict[str, Any] | None:
    if not monitor_path.is_file():
        return None
    rows = []
    for line in monitor_path.read_text(encoding="utf-8", errors="replace").splitlines():
        try:
            rows.append(json.loads(line))
        except json.JSONDecodeError:
            pass
    pools_dispatch = []
    pools_rl = []
    for r in rows:
        h = r.get("health")
        if isinstance(h, dict) and isinstance(h.get("pools"), dict):
            pools_dispatch.append(int(h["pools"].get("dispatch", 0)))
            pools_rl.append(int(h["pools"].get("ratelimited", 0)))
    return {
        "samples": len(rows),
        "dispatch_min": min(pools_dispatch) if pools_dispatch else None,
        "dispatch_max": max(pools_dispatch) if pools_dispatch else None,
        "ratelimited_max": max(pools_rl) if pools_rl else None,
        "ratelimited_nonzero_samples": sum(1 for x in pools_rl if x > 0),
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--run-dir", required=True)
    parser.add_argument("--spec", required=True)
    parser.add_argument("--monitor", default="")
    args = parser.parse_args()

    run_dir = Path(args.run_dir)
    spec = json.loads(Path(args.spec).read_text(encoding="utf-8"))
    report: dict[str, Any] = {
        "run_dir": str(run_dir),
        "events": analyze_events(run_dir / "events.jsonl"),
        "outputs": analyze_raw_outputs(run_dir, spec),
        "summary": load_json(run_dir / "summary.json"),
        "verification": load_json(run_dir / "verification_gate.json"),
        "ch109": load_json(run_dir / "ch109_acceptance.json"),
        "detailed": load_json(run_dir / "detailed_report.json"),
        "monitor": analyze_monitor(Path(args.monitor)) if args.monitor else None,
    }

    # Jitter severity heuristic
    jitter = report["events"].get("elapsed_ms", {}).get("p95") or 0
    p50 = report["events"].get("elapsed_ms", {}).get("p50") or 0
    report["jitter_assessment"] = {
        "p95_minus_p50_ms": (jitter - p50) if jitter and p50 else None,
        "severe": (jitter - p50) > 120000 if jitter and p50 else False,
    }

    out = run_dir / "comprehensive_analysis.json"
    out.write_text(json.dumps(report, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    print(json.dumps(report, indent=2, ensure_ascii=False))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
