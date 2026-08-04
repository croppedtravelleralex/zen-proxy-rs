#!/usr/bin/env python3
"""Daily dev cache test: short/mid/long context, same Pi session, probe turns 2+.

8 workers pinned to 8 cases. Each worker:
  1. Runs warmup turn(s) once (read/bash/bulk load — normal dev)
  2. Loops short follow-up probes with --continue until duration elapses

Gate: probe-turn Pi usage cacheRead / (input+cacheRead+cacheWrite) >= min_probe_cache_pct.
Optional ch109 window join for NewAPI cached_tokens / prompt_tokens tiers.
"""

from __future__ import annotations

import argparse
import dataclasses
import datetime as dt
import json
import os
import sys
import threading
import time
from concurrent.futures import ThreadPoolExecutor, as_completed
from pathlib import Path
from typing import Any

_SCRIPT_DIR = Path(__file__).resolve().parent
sys.path.insert(0, str(_SCRIPT_DIR))

from run_pi_bulk_quality_gate import run_pi_session_turn  # noqa: E402
from run_pi_matrix_8x5_test import (  # noqa: E402
    percentile,
    run_ch109_acceptance,
)
from run_piagent_parallel_rpm_test import (  # noqa: E402
    DEFAULT_PI,
    DEFAULT_THINKING,
    RUN_ROOT,
    TaskResult,
    sha256_text,
)
MATRIX_DIR = Path(__file__).resolve().parent / "pi-matrix"
DEFAULT_SPEC = MATRIX_DIR / "TASK_SPEC_DAILY_SESSION.json"


@dataclasses.dataclass
class DailyCase:
    id: str
    tier: str
    cwd: str
    timeout_s: int
    target_context_tokens: int
    warmup_turns: list[dict[str, Any]]
    probe_prompt: str


def load_cases(spec_path: Path) -> list[DailyCase]:
    spec = json.loads(spec_path.read_text(encoding="utf-8"))
    cases: list[DailyCase] = []
    for row in spec["cases"]:
        cases.append(
            DailyCase(
                id=row["id"],
                tier=row["tier"],
                cwd=row["cwd"],
                timeout_s=int(row.get("timeout_s", 600)),
                target_context_tokens=int(row.get("target_context_tokens", 0)),
                warmup_turns=row.get("warmup_turns", []),
                probe_prompt=row["probe_prompt"],
            )
        )
    return cases


def pi_cache_pct(stdout: str, stderr: str) -> dict[str, Any]:
    inp = 0
    cache = 0
    cache_write = 0
    for line in (stdout + "\n" + stderr).splitlines():
        line = line.strip()
        if not line.startswith("{") or not line.endswith("}"):
            continue
        try:
            obj = json.loads(line)
        except json.JSONDecodeError:
            continue
        if obj.get("type") not in ("message_end", "turn_end"):
            continue
        msg = obj.get("message") or {}
        usage = msg.get("usage") if isinstance(msg, dict) else None
        if not usage:
            continue
        inp = int(usage.get("input") or 0)
        cache = int(usage.get("cacheRead") or 0)
        cache_write = int(usage.get("cacheWrite") or 0)
    total = inp + cache + cache_write
    basis = inp + cache
    pct = round(100.0 * cache / basis, 2) if basis > 0 else 0.0
    return {
        "input": inp,
        "cacheRead": cache,
        "context_total": total,
        "pi_cache_pct": pct,
    }


def run_one_turn(
    case: DailyCase,
    *,
    turn_id: str,
    prompt: str,
    continue_session: bool,
    pi_bin: str,
    thinking: str,
    session_id: str,
    session_dir: Path,
    timeout_s: int,
    out_path: Path,
    err_path: Path,
) -> tuple[TaskResult, dict[str, Any]]:
    exit_code, timed_out, stdout, stderr, elapsed_ms = run_pi_session_turn(
        prompt=prompt,
        pi_bin=pi_bin,
        thinking=thinking,
        cwd=case.cwd,
        session_id=session_id,
        session_dir=session_dir,
        timeout_s=timeout_s,
        continue_session=continue_session,
        out_path=out_path,
        err_path=err_path,
    )
    cache_stats = pi_cache_pct(stdout, stderr)
    row = TaskResult(
        seq=0,
        case_id=case.id,
        cwd=case.cwd,
        started_at=dt.datetime.now(dt.timezone.utc).isoformat(),
        elapsed_ms=elapsed_ms,
        exit_code=exit_code,
        timed_out=timed_out,
        stdout_sha256=sha256_text(stdout),
        stderr_sha256=sha256_text(stderr),
        stdout_bytes=len(stdout.encode("utf-8", errors="replace")),
        stderr_bytes=len(stderr.encode("utf-8", errors="replace")),
        stdout_path=str(out_path),
        stderr_path=str(err_path),
        json_events=stdout.count("{"),
        assistant_text_chars=len(stdout),
        tool_events=stdout.count('"type":"tool"') + stdout.count('"type": "tool"'),
        subagent_markers=stdout.count('"subagent_type"'),
        error_markers=stdout.lower().count("error"),
        semantic_ok=exit_code == 0 and not timed_out,
    )
    return row, cache_stats


def worker_loop(
    worker_id: int,
    case: DailyCase,
    *,
    deadline: float,
    pi_bin: str,
    thinking: str,
    run_dir: Path,
    events_path: Path,
    events_lock: threading.Lock,
    seq_base: int,
) -> list[dict[str, Any]]:
    session_dir = run_dir / "sessions" / case.id
    session_dir.mkdir(parents=True, exist_ok=True)
    session_id = f"daily-session-{case.id}"
    case_raw = run_dir / "raw" / case.id
    case_raw.mkdir(parents=True, exist_ok=True)
    events: list[dict[str, Any]] = []
    continue_session = False
    seq = seq_base

    for idx, turn in enumerate(case.warmup_turns):
        if time.monotonic() >= deadline:
            break
        remaining = max(30, int(deadline - time.monotonic()))
        timeout_s = min(case.timeout_s, remaining)
        turn_id = turn["id"]
        out_path = case_raw / f"warmup-{turn_id}.stdout"
        err_path = case_raw / f"warmup-{turn_id}.stderr"
        row, cache_stats = run_one_turn(
            case,
            turn_id=f"warmup_{turn_id}",
            prompt=turn["prompt"],
            continue_session=continue_session,
            pi_bin=pi_bin,
            thinking=thinking,
            session_id=session_id,
            session_dir=session_dir,
            timeout_s=timeout_s,
            out_path=out_path,
            err_path=err_path,
        )
        continue_session = True
        event = {
            "event": "warmup_done",
            "worker_id": worker_id,
            "case_id": case.id,
            "tier": case.tier,
            "turn_id": turn_id,
            "turn_index": idx,
            "kind": "warmup",
            "seq": seq,
            "elapsed_ms": row.elapsed_ms,
            "exit_code": row.exit_code,
            "timed_out": row.timed_out,
            **cache_stats,
        }
        events.append(event)
        seq += 1
        with events_lock:
            with events_path.open("a", encoding="utf-8") as fh:
                fh.write(json.dumps(event, ensure_ascii=False) + "\n")
        print(json.dumps(event, ensure_ascii=False))
        if row.timed_out or row.exit_code != 0:
            return events
        min_ctx = turn.get("min_context_tokens")
        if min_ctx and cache_stats["context_total"] < int(min_ctx):
            event = {
                "event": "warmup_fail",
                "case_id": case.id,
                "reason": f"context_total {cache_stats['context_total']} < {min_ctx}",
            }
            events.append(event)
            with events_lock:
                with events_path.open("a", encoding="utf-8") as fh:
                    fh.write(json.dumps(event, ensure_ascii=False) + "\n")
            print(json.dumps(event, ensure_ascii=False))
            return events

    probe_idx = 0
    while time.monotonic() < deadline:
        remaining = deadline - time.monotonic()
        if remaining < 15.0:
            break
        timeout_s = min(case.timeout_s, int(remaining))
        if timeout_s < 30:
            break
        out_path = case_raw / f"probe-{probe_idx:04d}.stdout"
        err_path = case_raw / f"probe-{probe_idx:04d}.stderr"
        row, cache_stats = run_one_turn(
            case,
            turn_id=f"probe_{probe_idx}",
            prompt=case.probe_prompt,
            continue_session=continue_session,
            pi_bin=pi_bin,
            thinking=thinking,
            session_id=session_id,
            session_dir=session_dir,
            timeout_s=timeout_s,
            out_path=out_path,
            err_path=err_path,
        )
        event = {
            "event": "probe_done",
            "worker_id": worker_id,
            "case_id": case.id,
            "tier": case.tier,
            "probe_index": probe_idx,
            "kind": "probe",
            "seq": seq,
            "elapsed_ms": row.elapsed_ms,
            "exit_code": row.exit_code,
            "timed_out": row.timed_out,
            **cache_stats,
        }
        events.append(event)
        seq += 1
        probe_idx += 1
        with events_lock:
            with events_path.open("a", encoding="utf-8") as fh:
                fh.write(json.dumps(event, ensure_ascii=False) + "\n")
        print(json.dumps(event, ensure_ascii=False))
        if row.timed_out:
            break

    return events


def summarize_events(events: list[dict[str, Any]], min_pct: float) -> dict[str, Any]:
    probes = [e for e in events if e.get("kind") == "probe" and e.get("exit_code") == 0]
    by_tier: dict[str, list[dict[str, Any]]] = {}
    for e in events:
        by_tier.setdefault(e.get("tier", "unknown"), []).append(e)
    tier_summaries = []
    for tier, rows in sorted(by_tier.items()):
        tier_probes = [r for r in rows if r.get("kind") == "probe" and r.get("exit_code") == 0]
        pcts = [float(r["pi_cache_pct"]) for r in tier_probes if r.get("pi_cache_pct") is not None]
        elapsed = [float(r["elapsed_ms"]) for r in tier_probes if r.get("elapsed_ms")]
        tier_summaries.append(
            {
                "tier": tier,
                "probe_count": len(tier_probes),
                "pi_cache_pct": {
                    "mean": round(sum(pcts) / len(pcts), 2) if pcts else None,
                    "min": min(pcts) if pcts else None,
                    "p50": percentile(pcts, 50),
                    "p95": percentile(pcts, 95),
                    "p99": percentile(pcts, 99),
                },
                "probe_elapsed_ms": {
                    "p50": percentile(elapsed, 50),
                    "p95": percentile(elapsed, 95),
                    "p99": percentile(elapsed, 99),
                    "samples": len(elapsed),
                },
                "gate_pass": bool(pcts) and min(pcts) >= min_pct,
            }
        )
    all_pcts = [float(r["pi_cache_pct"]) for r in probes if r.get("pi_cache_pct") is not None]
    all_elapsed = [float(r["elapsed_ms"]) for r in probes if r.get("elapsed_ms")]
    return {
        "probe_total": len(probes),
        "min_probe_cache_pct": min_pct,
        "pi_cache_pct_all_probes": {
            "mean": round(sum(all_pcts) / len(all_pcts), 2) if all_pcts else None,
            "min": min(all_pcts) if all_pcts else None,
            "p50": percentile(all_pcts, 50),
            "p95": percentile(all_pcts, 95),
            "p99": percentile(all_pcts, 99),
            "samples": len(all_pcts),
        },
        "probe_elapsed_ms_all": {
            "p50": percentile(all_elapsed, 50),
            "p95": percentile(all_elapsed, 95),
            "p99": percentile(all_elapsed, 99),
            "samples": len(all_elapsed),
        },
        "gate_pass": bool(all_pcts) and min(all_pcts) >= min_pct,
        "tiers": tier_summaries,
        "per_case": _per_case_summary(events, min_pct),
    }


def _per_case_summary(events: list[dict[str, Any]], min_pct: float) -> list[dict[str, Any]]:
    by_case: dict[str, list[dict[str, Any]]] = {}
    for e in events:
        by_case.setdefault(e.get("case_id", ""), []).append(e)
    out = []
    for case_id, rows in sorted(by_case.items()):
        probes = [r for r in rows if r.get("kind") == "probe" and r.get("exit_code") == 0]
        pcts = [float(r["pi_cache_pct"]) for r in probes]
        out.append(
            {
                "case_id": case_id,
                "tier": rows[0].get("tier"),
                "probe_count": len(probes),
                "pi_cache_pct_min": min(pcts) if pcts else None,
                "pi_cache_pct_mean": round(sum(pcts) / len(pcts), 2) if pcts else None,
                "gate_pass": bool(pcts) and min(pcts) >= min_pct,
            }
        )
    return out


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--spec", default=str(DEFAULT_SPEC))
    parser.add_argument("--pi-bin", default=DEFAULT_PI)
    parser.add_argument("--thinking", default=os.environ.get("PI_TEST_THINKING", "high"))
    parser.add_argument("--duration-minutes", type=float, default=10.0)
    parser.add_argument("--shutdown-grace-s", type=float, default=300.0)
    parser.add_argument("--run-dir", default="")
    parser.add_argument("--skip-setup-check", action="store_true")
    parser.add_argument("--run-ch109-acceptance", action="store_true")
    parser.add_argument("--ch109-label", default="pi-daily-session")
    args = parser.parse_args(argv)

    spec_path = Path(args.spec)
    spec = json.loads(spec_path.read_text(encoding="utf-8"))
    min_pct = float(spec.get("min_probe_cache_pct", 99.0))
    cases = load_cases(spec_path)
    if len(cases) != 8:
        print(f"expected 8 cases, got {len(cases)}", file=sys.stderr)
        return 2

    missing = [c.cwd for c in cases if not Path(c.cwd).exists()]
    if missing and not args.skip_setup_check:
        print(f"fixture dirs missing: {missing}", file=sys.stderr)
        return 2

    stamp = dt.datetime.now().strftime("%Y%m%d-%H%M%S")
    run_dir = Path(args.run_dir) if args.run_dir else RUN_ROOT / f"pi-daily-session-{stamp}"
    run_dir.mkdir(parents=True, exist_ok=True)
    events_path = run_dir / "events.jsonl"
    events_path.write_text("", encoding="utf-8")

    wall_start_epoch = int(time.time())
    wall_start = time.monotonic()
    deadline = wall_start + args.duration_minutes * 60.0

    meta = {
        "created_at": dt.datetime.now(dt.timezone.utc).isoformat(),
        "spec": str(spec_path),
        "session_mode": True,
        "duration_minutes": args.duration_minutes,
        "min_probe_cache_pct": min_pct,
        "workers": 8,
        "route": "closeTest -> NewAPI ch109 -> panda :4010 -> zen-proxy-test :4011",
        "thinking": args.thinking,
        "cases": [{"id": c.id, "tier": c.tier, "cwd": c.cwd} for c in cases],
    }
    (run_dir / "meta.json").write_text(json.dumps(meta, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    print(json.dumps({"event": "start", "run_dir": str(run_dir), **meta}, ensure_ascii=False))

    all_events: list[dict[str, Any]] = []
    events_lock = threading.Lock()
    with ThreadPoolExecutor(max_workers=8) as pool:
        futures = {
            pool.submit(
                worker_loop,
                i,
                case,
                deadline=deadline,
                pi_bin=args.pi_bin,
                thinking=args.thinking,
                run_dir=run_dir,
                events_path=events_path,
                events_lock=events_lock,
                seq_base=i * 1000,
            ): case
            for i, case in enumerate(cases)
        }
        for fut in as_completed(futures):
            case = futures[fut]
            try:
                rows = fut.result()
                all_events.extend(rows)
                print(json.dumps({"event": "worker_done", "case_id": case.id, "events": len(rows)}))
            except Exception as exc:  # noqa: BLE001
                print(json.dumps({"event": "worker_error", "case_id": case.id, "error": str(exc)}))

    wall_end_epoch = int(time.time())
    wall_ms = int((time.monotonic() - wall_start) * 1000)

    cache_summary = summarize_events(all_events, min_pct)
    (run_dir / "cache_summary.json").write_text(
        json.dumps(cache_summary, indent=2, ensure_ascii=False) + "\n", encoding="utf-8"
    )

    ch109: dict[str, Any] | None = None
    if args.run_ch109_acceptance:
        ch109 = run_ch109_acceptance(run_dir, wall_start_epoch, wall_end_epoch, args.ch109_label)
        if ch109 and ch109.get("newapi"):
            ps = int(ch109["newapi"].get("prompt_sum") or 0)
            cs = int(ch109["newapi"].get("cache_sum") or 0)
            ch109["newapi"]["true_cache_hit_pct"] = round(100.0 * cs / (ps + cs), 2) if ps + cs else 0.0

    final = {
        "event": "done",
        "run_dir": str(run_dir),
        "wall_ms": wall_ms,
        "wall_start_epoch": wall_start_epoch,
        "wall_end_epoch": wall_end_epoch,
        "cache_gate_pass": cache_summary["gate_pass"],
        "cache_summary": cache_summary,
        "ch109": ch109,
    }
    (run_dir / "final.json").write_text(json.dumps(final, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    print(json.dumps(final, ensure_ascii=False))
    return 0 if cache_summary["gate_pass"] else 1


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
