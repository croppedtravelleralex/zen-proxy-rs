#!/usr/bin/env python3
"""8 isolated dirs × 8 fixed tasks × 8 concurrent workers × ≥5 minutes.

Each worker is pinned to one case (directory + prompt). Iterations loop until
wall-clock duration elapses. All stdout/stderr and per-iteration metadata are
recorded. After the run:

1. DeepSeek (via Pi closeTest / ch109) self-checks via SELF_CHECK_PASS/FAIL in prompt.
2. verify_matrix_outputs.py applies deterministic gate rules.
3. Optional ch109 NewAPI window join (--run-ch109-acceptance).

Does not touch production zen-proxy 4001/4002/4004.
"""

from __future__ import annotations

import argparse
import dataclasses
import datetime as dt
import json
import os
import subprocess
import sys
import threading
import time
from concurrent.futures import Future, ThreadPoolExecutor
from pathlib import Path
from typing import Any

_SCRIPT_DIR = Path(__file__).resolve().parent
sys.path.insert(0, str(_SCRIPT_DIR))

# Reuse Pi runner helpers from RPM test harness.
from run_piagent_parallel_rpm_test import (
    DEFAULT_PI,
    DEFAULT_THINKING,
    RUN_ROOT,
    RateLimiter,
    TaskResult,
    TaskSpec,
    run_pi_task,
    sha256_text,
    wait_any,
)

MATRIX_DIR = Path(__file__).resolve().parent / "pi-matrix"
DEFAULT_SPEC = MATRIX_DIR / "TASK_SPEC.json"
VERIFY_SCRIPT = MATRIX_DIR / "verify_matrix_outputs.py"


@dataclasses.dataclass
class MatrixCase:
    id: str
    dir_name: str
    cwd: str
    prompt: str
    timeout_s: int
    target_context_tokens: int | None = None


def load_cases(spec_path: Path, self_check_suffix: str) -> list[MatrixCase]:
    spec = json.loads(spec_path.read_text(encoding="utf-8"))
    root = spec["root"]
    suffix = spec.get("self_check_suffix", self_check_suffix)
    cases: list[MatrixCase] = []
    for row in spec["cases"]:
        cases.append(
            MatrixCase(
                id=row["id"],
                dir_name=row["dir"],
                cwd=str(Path(root) / row["dir"]),
                prompt=row["prompt"] + suffix,
                timeout_s=int(row.get("timeout_s", 360)),
                target_context_tokens=row.get("target_context_tokens"),
            )
        )
    return cases


def percentile(values: list[float], p: float) -> float | None:
    if not values:
        return None
    s = sorted(values)
    i = round((p / 100.0) * (len(s) - 1))
    return s[max(0, min(i, len(s) - 1))]


def copy_last_for_gate(case_id: str, run_dir: Path) -> None:
    case_raw = run_dir / "raw" / case_id
    last = run_dir / "raw" / f"{case_id}.stdout"
    err_last = run_dir / "raw" / f"{case_id}.stderr"
    if not case_raw.exists():
        return
    iters = sorted(case_raw.glob("iter-*.stdout"))
    if not iters:
        return
    last.write_text(iters[-1].read_text(encoding="utf-8", errors="replace"), encoding="utf-8")
    err_src = iters[-1].with_suffix(".stderr")
    if err_src.exists():
        err_last.write_text(err_src.read_text(encoding="utf-8", errors="replace"), encoding="utf-8")


def worker_loop(
    worker_id: int,
    case: MatrixCase,
    *,
    deadline: float,
    pi_bin: str,
    thinking: str,
    run_dir: Path,
    events_lock: threading.Lock,
    events_path: Path,
    seq_counter: list[int],
    seq_lock: threading.Lock,
) -> list[TaskResult]:
    rows: list[TaskResult] = []
    case_raw = run_dir / "raw" / case.id
    case_raw.mkdir(parents=True, exist_ok=True)
    while time.monotonic() < deadline:
        remaining_s = deadline - time.monotonic()
        if remaining_s < 5.0:
            break
        iteration_timeout_s = min(case.timeout_s, int(remaining_s))
        if iteration_timeout_s < case.timeout_s and iteration_timeout_s < 30:
            break
        with seq_lock:
            seq = seq_counter[0]
            seq_counter[0] += 1
        spec = TaskSpec(
            seq=seq,
            case_id=case.id,
            cwd=case.cwd,
            prompt=case.prompt,
            timeout_s=iteration_timeout_s,
        )
        # Save under case subdir; run_pi_task writes to raw/{seq}-... — we relocate after.
        row = run_pi_task(spec, pi_bin=pi_bin, thinking=thinking, run_dir=run_dir)
        iter_tag = f"iter-{len(rows):04d}"
        src_out = Path(row.stdout_path)
        src_err = Path(row.stderr_path)
        dst_out = case_raw / f"{iter_tag}.stdout"
        dst_err = case_raw / f"{iter_tag}.stderr"
        if src_out.exists():
            dst_out.write_text(src_out.read_text(encoding="utf-8", errors="replace"), encoding="utf-8")
        if src_err.exists():
            dst_err.write_text(src_err.read_text(encoding="utf-8", errors="replace"), encoding="utf-8")
        row.stdout_path = str(dst_out)
        row.stderr_path = str(dst_err)
        rows.append(row)
        event = {
            "event": "iteration",
            "worker_id": worker_id,
            "case_id": case.id,
            "seq": row.seq,
            "iteration": len(rows) - 1,
            "started_at": row.started_at,
            "elapsed_ms": row.elapsed_ms,
            "exit_code": row.exit_code,
            "timed_out": row.timed_out,
            "semantic_ok": row.semantic_ok,
            "tool_events": row.tool_events,
            "subagent_markers": row.subagent_markers,
            "error_markers": row.error_markers,
            "stdout_sha256": row.stdout_sha256,
            "stderr_sha256": row.stderr_sha256,
            "stdout_path": row.stdout_path,
            "stderr_path": row.stderr_path,
        }
        with events_lock:
            with events_path.open("a", encoding="utf-8") as fh:
                fh.write(json.dumps(event, ensure_ascii=False) + "\n")
        print(json.dumps(event, ensure_ascii=False))
        if time.monotonic() >= deadline:
            break
    return rows


def summarize_matrix(all_rows: list[TaskResult], wall_ms: int, cases: list[MatrixCase]) -> dict[str, Any]:
    by_case: dict[str, list[TaskResult]] = {c.id: [] for c in cases}
    for row in all_rows:
        by_case.setdefault(row.case_id, []).append(row)
    case_summaries = []
    for case in cases:
        rows = by_case.get(case.id, [])
        ok_exit = [r for r in rows if r.exit_code == 0 and not r.timed_out]
        elapsed = [float(r.elapsed_ms) for r in rows if r.elapsed_ms > 0]
        case_summaries.append(
            {
                "case_id": case.id,
                "target_context_tokens": case.target_context_tokens,
                "cwd": case.cwd,
                "iterations": len(rows),
                "ok_exit_iterations": len(ok_exit),
                "last_elapsed_ms": rows[-1].elapsed_ms if rows else None,
                "last_semantic_ok": rows[-1].semantic_ok if rows else False,
                "total_tool_events": sum(r.tool_events for r in rows),
                "elapsed_ms": {
                    "p50": percentile(elapsed, 50),
                    "p95": percentile(elapsed, 95),
                    "p99": percentile(elapsed, 99),
                    "samples": len(elapsed),
                },
            }
        )
    all_elapsed = [float(r.elapsed_ms) for r in all_rows if r.elapsed_ms > 0]
    timeout_rows = [r for r in all_rows if r.timed_out]
    subagent_rows = [r for r in all_rows if r.subagent_markers > 0]
    return {
        "total_iterations": len(all_rows),
        "timeout_iterations": len(timeout_rows),
        "subagent_violations": len(subagent_rows),
        "wall_ms": wall_ms,
        "iterations_per_min": len(all_rows) / max(wall_ms / 60000.0, 1e-9),
        "elapsed_ms_all": {
            "p50": percentile(all_elapsed, 50),
            "p95": percentile(all_elapsed, 95),
            "p99": percentile(all_elapsed, 99),
            "samples": len(all_elapsed),
        },
        "cases": case_summaries,
    }


def build_detailed_report(
    run_dir: Path,
    spec_path: Path,
    summary: dict[str, Any],
    gate_pass: bool,
    verification: dict[str, Any] | None,
    ch109: dict[str, Any] | None,
) -> dict[str, Any]:
    spec = json.loads(spec_path.read_text(encoding="utf-8"))
    quality_by_case = {}
    if verification:
        for row in verification.get("cases", []):
            quality_by_case[row["case_id"]] = {
                "gate_pass": row.get("pass"),
                "issues": row.get("issues", []),
                "quality": row.get("quality", {}),
                "selected_stdout": row.get("selected_stdout"),
            }
    return {
        "run_dir": str(run_dir),
        "spec": str(spec_path),
        "target_token_tiers": sorted(
            {c.get("target_context_tokens") for c in spec.get("cases", []) if c.get("target_context_tokens")}
        ),
        "gate_pass": gate_pass,
        "matrix_gate": {
            "pass_count": verification.get("pass_count") if verification else None,
            "fail_count": verification.get("fail_count") if verification else None,
            "policy": verification.get("gate_policy") if verification else None,
        },
        "output_quality": quality_by_case,
        "latency_pi_elapsed_ms": summary.get("elapsed_ms_all"),
        "latency_per_case": {
            c["case_id"]: c.get("elapsed_ms") for c in summary.get("cases", [])
        },
        "ch109_newapi": (ch109 or {}).get("newapi"),
        "ch109_frt_ms": (ch109 or {}).get("newapi", {}).get("frt_ms"),
        "ch109_cache_pct_token_weighted": (ch109 or {}).get("newapi", {}).get("cache_pct_token_weighted"),
        "ch109_gate": (ch109 or {}).get("gate"),
        "ch109_audit": (ch109 or {}).get("audit"),
    }


def resolve_ch109_baseline() -> Path | None:
    candidates = [
        Path(__file__).resolve().parent / "docs" / "baselines" / "ch109-fix3-pi.json",
        Path("//wsl.localhost/HermesUbuntu/home/lenovo/zen-free-model-suite/docs/baselines/ch109-fix3-pi.json"),
    ]
    env_root = os.environ.get("ZEN_SUITE_ROOT")
    if env_root:
        candidates.insert(0, Path(env_root) / "docs/baselines/ch109-fix3-pi.json")
    for path in candidates:
        if path.is_file():
            return path
    return None


def run_ch109_acceptance(run_dir: Path, wall_start_epoch: int, wall_end_epoch: int, label: str) -> dict[str, Any] | None:
    script = Path(__file__).resolve().parent / "run_ch109_acceptance_window.py"
    out = run_dir / "ch109_acceptance.json"
    cmd = [
        sys.executable,
        str(script),
        "--since-epoch",
        str(wall_start_epoch),
        "--until-epoch",
        str(wall_end_epoch),
        "--label",
        label,
        "--out",
        str(out),
    ]
    baseline = resolve_ch109_baseline()
    if baseline is not None:
        cmd.extend(["--baseline-file", str(baseline)])
    print(json.dumps({"event": "ch109_acceptance_start", "cmd": cmd}, ensure_ascii=False))
    proc = subprocess.run(cmd, capture_output=True, text=True, encoding="utf-8", errors="replace")
    (run_dir / "ch109_acceptance.stdout").write_text(proc.stdout, encoding="utf-8")
    (run_dir / "ch109_acceptance.stderr").write_text(proc.stderr, encoding="utf-8")
    if out.exists():
        return json.loads(out.read_text(encoding="utf-8"))
    if proc.returncode != 0:
        print(
            json.dumps(
                {
                    "event": "ch109_acceptance_failed",
                    "returncode": proc.returncode,
                    "stderr_tail": (proc.stderr or "")[-500:],
                },
                ensure_ascii=False,
            ),
            file=sys.stderr,
        )
    return None


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--spec", default=str(DEFAULT_SPEC))
    parser.add_argument("--pi-bin", default=DEFAULT_PI)
    parser.add_argument("--thinking", default=DEFAULT_THINKING)
    parser.add_argument("--duration-minutes", type=float, default=5.0)
    parser.add_argument("--shutdown-grace-s", type=float, default=60.0)
    parser.add_argument("--target-rpm", type=float, default=0.0, help="0 = no global limiter (8 workers free)")
    parser.add_argument("--run-dir", default="")
    parser.add_argument("--skip-setup-check", action="store_true")
    parser.add_argument("--run-ch109-acceptance", action="store_true")
    parser.add_argument("--ch109-label", default="pi-matrix-8x5")
    args = parser.parse_args(argv)

    spec_path = Path(args.spec)
    spec_json = json.loads(spec_path.read_text(encoding="utf-8"))
    strict_no_subagent = bool(spec_json.get("strict_no_subagent", False))
    cases = load_cases(spec_path, "")
    if len(cases) != 8:
        print(f"expected 8 cases in spec, got {len(cases)}", file=sys.stderr)
        return 2

    missing = [c.cwd for c in cases if not Path(c.cwd).exists()]
    if missing and not args.skip_setup_check:
        setup_hint = (
            "  python3 ops/local-dev/pi-matrix/setup_large_matrix_fixtures.py\n"
            if "large" in spec_path.name.lower()
            else "  powershell -ExecutionPolicy Bypass -File ops/local-dev/pi-matrix/setup_matrix_dirs.ps1\n"
        )
        print(
            "fixture dirs missing; run first:\n" + setup_hint + f"missing: {missing}",
            file=sys.stderr,
        )
        return 2

    stamp = dt.datetime.now().strftime("%Y%m%d-%H%M%S")
    run_dir = Path(args.run_dir) if args.run_dir else RUN_ROOT / f"pi-matrix-8x5-{stamp}"
    run_dir.mkdir(parents=True, exist_ok=True)
    events_path = run_dir / "events.jsonl"
    events_path.write_text("", encoding="utf-8")

    wall_start_epoch = int(time.time())
    wall_start = time.monotonic()
    deadline = wall_start + args.duration_minutes * 60.0

    meta = {
        "created_at": dt.datetime.now(dt.timezone.utc).isoformat(),
        "pi_bin": args.pi_bin,
        "workers": 8,
        "duration_minutes": args.duration_minutes,
        "thinking": args.thinking,
        "spec": str(spec_path),
        "route": "closeTest -> NewAPI channel 109 -> panda :4010 -> zen-proxy-test :4011",
        "production_touched": False,
        "cases": [{"id": c.id, "cwd": c.cwd} for c in cases],
    }
    (run_dir / "meta.json").write_text(json.dumps(meta, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    print(json.dumps({"event": "start", "run_dir": str(run_dir), **meta}, ensure_ascii=False))

    limiter = RateLimiter(args.target_rpm) if args.target_rpm > 0 else None
    events_lock = threading.Lock()
    seq_counter = [0]
    seq_lock = threading.Lock()
    all_rows: list[TaskResult] = []
    futures: dict[Future[list[TaskResult]], int] = {}

    with ThreadPoolExecutor(max_workers=8) as pool:
        for worker_id, case in enumerate(cases):
            if limiter:
                limiter.acquire()
            fut = pool.submit(
                worker_loop,
                worker_id,
                case,
                deadline=deadline,
                pi_bin=args.pi_bin,
                thinking=args.thinking,
                run_dir=run_dir,
                events_lock=events_lock,
                events_path=events_path,
                seq_counter=seq_counter,
                seq_lock=seq_lock,
            )
            futures[fut] = worker_id

        grace_deadline = deadline + args.shutdown_grace_s
        while futures and time.monotonic() < grace_deadline:
            done, _ = wait_any(futures, timeout=1.0)
            for fut in done:
                worker_id = futures.pop(fut)
                try:
                    rows = fut.result()
                    all_rows.extend(rows)
                    print(json.dumps({"event": "worker_done", "worker_id": worker_id, "iterations": len(rows)}))
                except Exception as exc:  # noqa: BLE001
                    print(json.dumps({"event": "worker_error", "worker_id": worker_id, "error": str(exc)}))

    wall_ms = int((time.monotonic() - wall_start) * 1000)
    wall_end_epoch = int(time.time())

    summary = summarize_matrix(all_rows, wall_ms, cases)
    (run_dir / "summary.json").write_text(json.dumps(summary, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")

    verify_cmd = [sys.executable, str(VERIFY_SCRIPT), "--run-dir", str(run_dir), "--spec", str(spec_path)]
    print(json.dumps({"event": "verify_gate_start", "cmd": verify_cmd}, ensure_ascii=False))
    gate_proc = subprocess.run(verify_cmd, capture_output=True, text=True, encoding="utf-8", errors="replace")
    (run_dir / "verify_gate.stdout").write_text(gate_proc.stdout, encoding="utf-8")
    (run_dir / "verify_gate.stderr").write_text(gate_proc.stderr, encoding="utf-8")
    gate_pass = gate_proc.returncode == 0 and summary.get("timeout_iterations", 0) == 0
    if strict_no_subagent and summary.get("subagent_violations", 0) > 0:
        gate_pass = False
    verification: dict[str, Any] | None = None
    verify_path = run_dir / "verification_gate.json"
    if verify_path.exists():
        verification = json.loads(verify_path.read_text(encoding="utf-8"))

    ch109: dict[str, Any] | None = None
    if args.run_ch109_acceptance:
        ch109 = run_ch109_acceptance(run_dir, wall_start_epoch, wall_end_epoch, args.ch109_label)

    detailed = build_detailed_report(run_dir, spec_path, summary, gate_pass, verification, ch109)
    (run_dir / "detailed_report.json").write_text(
        json.dumps(detailed, indent=2, ensure_ascii=False) + "\n", encoding="utf-8"
    )

    final = {
        "event": "done",
        "run_dir": str(run_dir),
        "wall_ms": wall_ms,
        "wall_start_epoch": wall_start_epoch,
        "wall_end_epoch": wall_end_epoch,
        "gate_pass": gate_pass,
        "summary": summary,
        "verification": {
            "pass_count": verification.get("pass_count") if verification else None,
            "fail_count": verification.get("fail_count") if verification else None,
        },
        "ch109_frt_ms": detailed.get("ch109_frt_ms"),
        "ch109_cache_pct_token_weighted": detailed.get("ch109_cache_pct_token_weighted"),
        "pi_elapsed_ms": summary.get("elapsed_ms_all"),
    }
    (run_dir / "final.json").write_text(json.dumps(final, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    print(json.dumps(final, ensure_ascii=False))
    return 0 if gate_pass else 1


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
