#!/usr/bin/env python3
"""Run parallel Pi coding-agent sessions against the closeTest / channel-109 path.

Constraints (user request):
- Up to 8 concurrent parent Pi processes (not production zen-proxy@4001/4002/4004).
- Prompt mix uses pi-subagents parallel/chain workflows to lift API RPM.
- Target 30-50 RPM aggregate API traffic for 5 minutes of daily-dev-shaped work.

Pi credentials and provider routing come from the local Pi agent dir
(%USERPROFILE%\\.pi\\agent), default provider closeTest -> NewAPI channel 109
-> panda :4010 -> zen-proxy-test :4011.

No API keys are written into result files.
"""

from __future__ import annotations

import argparse
import collections
import dataclasses
import datetime as dt
import hashlib
import json
import os
import re
import shlex
import signal
import subprocess
import sys
import threading
import time
from concurrent.futures import ThreadPoolExecutor, as_completed
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parents[2]
RUN_ROOT = ROOT / ".local-dev" / "runs"

DEFAULT_PI = os.environ.get(
    "PI_AGENT_BIN",
    r"C:\software\nodejs\node_global\pi.cmd",
)
DEFAULT_THINKING = os.environ.get("PI_TEST_THINKING", "high")

# Daily-dev shaped prompts: read-only project inspection + subagent delegation.
CASES: list[tuple[str, str, str, int]] = [
    (
        "short_ping",
        r"D:\SelfMadeTool\Tide",
        "Reply exactly DAILY_OK. No tools.",
        45,
    ),
    (
        "scout_tide",
        r"D:\SelfMadeTool\Tide",
        (
            "只读检查此项目目录结构与主要入口文件，不要修改任何文件。"
            "使用 scout 子代理做一次快速 recon，最后用 5 条要点汇报。"
        ),
        300,
    ),
    (
        "scout_mirofish",
        r"D:\SelfMadeTool\MiroFish",
        (
            "只读检查此项目并汇报架构与风险点。优先用 scout 子代理；"
            "若需要可用 parallel 模式让 scout 与 reviewer 并行各看不同方面。"
        ),
        300,
    ),
    (
        "parallel_review_outlook",
        r"D:\SelfMadeTool\AutoRegister\camoufoxOutlookRegister",
        (
            "只读检查此项目。用 parallel subagents：一个 scout 看目录与依赖，"
            "一个 reviewer 看潜在 bug，一个 reviewer 看测试覆盖。"
            "汇总成简短中文结论，不要改文件。"
        ),
        300,
    ),
    (
        "scout_personal",
        r"D:\SelfMadeTool\personal",
        (
            "只读检查此目录可清理项（不要删除/移动文件）。"
            "用 scout 子代理列出 top 5 可关注路径并说明理由。"
        ),
        300,
    ),
    (
        "bash_read_combo",
        r"D:\SelfMadeTool\Tide",
        (
            "用 bash 列出项目根目录一级文件，再用 read 打开 README 或 package 文件（若存在），"
            "最后回复 READ_OK 加一行摘要。只读，不要编辑。"
        ),
        90,
    ),
    (
        "chain_scout_planner",
        r"D:\SelfMadeTool\MiroFish",
        (
            "先让 scout 子代理只读摸清模块边界，再让 planner 子代理给出 3 步改进建议。"
            "使用 chain 模式。不要写文件。"
        ),
        300,
    ),
    (
        "short_json",
        r"D:\SelfMadeTool\Tide",
        "Reply with a single JSON object only: {\"status\":\"ok\",\"client\":\"pi_matrix\"}.",
        60,
    ),
]


@dataclasses.dataclass
class TaskSpec:
    seq: int
    case_id: str
    cwd: str
    prompt: str
    timeout_s: int


@dataclasses.dataclass
class TaskResult:
    seq: int
    case_id: str
    cwd: str
    started_at: str
    elapsed_ms: int
    exit_code: int | None
    timed_out: bool
    stdout_sha256: str
    stderr_sha256: str
    stdout_bytes: int
    stderr_bytes: int
    stdout_path: str
    stderr_path: str
    json_events: int
    assistant_text_chars: int
    tool_events: int
    subagent_markers: int
    error_markers: int
    semantic_ok: bool


class RateLimiter:
    def __init__(self, rpm: float) -> None:
        self._interval = 60.0 / max(rpm, 1.0)
        self._lock = threading.Lock()
        self._next = time.monotonic()

    def acquire(self) -> None:
        with self._lock:
            now = time.monotonic()
            if now < self._next:
                time.sleep(self._next - now)
            self._next = max(self._next + self._interval, time.monotonic())


def sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def sha256_text(value: str) -> str:
    return sha256_bytes(value.encode("utf-8", errors="replace"))


def parse_pi_json_output(text: str) -> dict[str, int]:
    events = 0
    assistant_chars = 0
    tool_events = 0
    subagent_markers = 0
    error_markers = 0
    for line in text.splitlines():
        line = line.strip()
        if not line.startswith("{") or not line.endswith("}"):
            continue
        try:
            obj = json.loads(line)
        except json.JSONDecodeError:
            continue
        events += 1
        blob = json.dumps(obj, ensure_ascii=False).lower()
        if "assistant" in blob and "content" in blob:
            assistant_chars += len(blob)
        if '"type":"tool"' in blob or '"type": "tool"' in blob:
            tool_events += 1
        if '"type":"toolcall"' in blob or '"type": "toolcall"' in blob:
            tool_events += 1
        if '"subagent_type"' in blob:
            subagent_markers += 1
        if "error" in blob or "failed" in blob:
            error_markers += 1
    return {
        "json_events": events,
        "assistant_text_chars": assistant_chars,
        "tool_events": tool_events,
        "subagent_markers": subagent_markers,
        "error_markers": error_markers,
    }


def semantic_ok_for_case(case_id: str, stdout: str, stderr: str) -> bool:
    merged = f"{stdout}\n{stderr}".lower()
    if case_id == "short_ping":
        return "daily_ok" in merged
    if case_id == "short_json":
        return '"status"' in merged and '"ok"' in merged
    if case_id == "bash_read_combo":
        return "read_ok" in merged
    if case_id.startswith("scout") or case_id.startswith("parallel") or case_id.startswith("chain"):
        return len(stdout.strip()) > 80 or "subagent" in merged
    return len(stdout.strip()) > 20


def kill_process_tree(pid: int) -> None:
    """Kill Pi and any scout/subagent children (Windows taskkill /T is required)."""
    if pid <= 0:
        return
    if os.name == "nt":
        subprocess.run(
            ["taskkill", "/F", "/T", "/PID", str(pid)],
            capture_output=True,
            text=True,
            check=False,
        )
        return
    try:
        os.killpg(os.getpgid(pid), signal.SIGKILL)
    except ProcessLookupError:
        return
    except OSError:
        try:
            os.kill(pid, signal.SIGKILL)
        except ProcessLookupError:
            return


def run_pi_task(
    spec: TaskSpec,
    *,
    pi_bin: str,
    thinking: str,
    run_dir: Path,
) -> TaskResult:
    out_path = run_dir / "raw" / f"{spec.seq:04d}-{spec.case_id}.stdout"
    err_path = run_dir / "raw" / f"{spec.seq:04d}-{spec.case_id}.stderr"
    out_path.parent.mkdir(parents=True, exist_ok=True)

    cmd = [
        pi_bin,
        "-p",
        spec.prompt,
        "--mode",
        "json",
        "--no-session",
        "--thinking",
        thinking,
    ]
    started = dt.datetime.now(dt.timezone.utc)
    timed_out = False
    exit_code: int | None = None
    popen_kwargs: dict[str, Any] = {
        "cwd": spec.cwd,
        "stdout": subprocess.PIPE,
        "stderr": subprocess.PIPE,
        "text": True,
        "encoding": "utf-8",
        "errors": "replace",
        "shell": False,
    }
    if os.name != "nt":
        popen_kwargs["start_new_session"] = True
    try:
        proc = subprocess.Popen(cmd, **popen_kwargs)
        try:
            stdout, stderr = proc.communicate(timeout=spec.timeout_s)
            stdout = stdout or ""
            stderr = stderr or ""
            exit_code = proc.returncode
        except subprocess.TimeoutExpired:
            kill_process_tree(proc.pid)
            try:
                stdout, stderr = proc.communicate(timeout=10)
            except subprocess.TimeoutExpired:
                stdout, stderr = "", ""
            stdout = stdout or ""
            stderr = stderr or ""
            timed_out = True
            exit_code = None
    except FileNotFoundError:
        stdout = ""
        stderr = f"pi binary not found: {pi_bin}"
        exit_code = 127

    out_path.write_text(stdout, encoding="utf-8")
    err_path.write_text(stderr, encoding="utf-8")
    parsed = parse_pi_json_output(stdout + "\n" + stderr)
    elapsed_ms = int((dt.datetime.now(dt.timezone.utc) - started).total_seconds() * 1000)
    return TaskResult(
        seq=spec.seq,
        case_id=spec.case_id,
        cwd=spec.cwd,
        started_at=started.isoformat(),
        elapsed_ms=elapsed_ms,
        exit_code=exit_code,
        timed_out=timed_out,
        stdout_sha256=sha256_text(stdout),
        stderr_sha256=sha256_text(stderr),
        stdout_bytes=len(stdout.encode("utf-8", errors="replace")),
        stderr_bytes=len(stderr.encode("utf-8", errors="replace")),
        stdout_path=str(out_path),
        stderr_path=str(err_path),
        json_events=parsed["json_events"],
        assistant_text_chars=parsed["assistant_text_chars"],
        tool_events=parsed["tool_events"],
        subagent_markers=parsed["subagent_markers"],
        error_markers=parsed["error_markers"],
        semantic_ok=semantic_ok_for_case(spec.case_id, stdout, stderr),
    )


def percentile(values: list[int], pct: int) -> int | None:
    if not values:
        return None
    ordered = sorted(values)
    idx = round((pct / 100) * (len(ordered) - 1))
    return ordered[max(0, min(idx, len(ordered) - 1))]


def summarize(results: list[TaskResult], wall_ms: int, target_rpm: float) -> dict[str, Any]:
    ok = [r for r in results if r.exit_code == 0 and not r.timed_out]
    timeouts = [r for r in results if r.timed_out]
    semantic = [r for r in results if r.semantic_ok]
    elapsed = [r.elapsed_ms for r in results]
    parent_rpm = len(results) / max(wall_ms / 60000.0, 1e-9)
    estimated_api_events = sum(r.tool_events + r.subagent_markers + 1 for r in results)
    api_rpm = estimated_api_events / max(wall_ms / 60000.0, 1e-9)
    by_case: dict[str, list[TaskResult]] = collections.defaultdict(list)
    for row in results:
        by_case[row.case_id].append(row)
    case_rows = []
    for case_id, rows in sorted(by_case.items()):
        case_rows.append(
            {
                "case_id": case_id,
                "count": len(rows),
                "ok": sum(1 for r in rows if r.exit_code == 0 and not r.timed_out),
                "semantic_ok": sum(1 for r in rows if r.semantic_ok),
                "timeouts": sum(1 for r in rows if r.timed_out),
                "p50_ms": percentile([r.elapsed_ms for r in rows], 50),
                "p90_ms": percentile([r.elapsed_ms for r in rows], 90),
                "subagent_markers": sum(r.subagent_markers for r in rows),
            }
        )
    return {
        "parent_sessions": len(results),
        "parent_ok": len(ok),
        "parent_timeouts": len(timeouts),
        "semantic_ok": len(semantic),
        "wall_ms": wall_ms,
        "parent_rpm": round(parent_rpm, 2),
        "estimated_api_events": estimated_api_events,
        "estimated_api_rpm": round(api_rpm, 2),
        "target_rpm": target_rpm,
        "rpm_within_target": target_rpm * 0.75 <= api_rpm <= target_rpm * 1.25,
        "elapsed_ms": {
            "p50": percentile(elapsed, 50),
            "p90": percentile(elapsed, 90),
            "p95": percentile(elapsed, 95),
            "max": max(elapsed) if elapsed else None,
        },
        "cases": case_rows,
    }


def collect_panda_channel_stats(
    channel_id: int,
    since_epoch_s: int,
    ssh_host: str,
) -> dict[str, Any]:
    sql = (
        "SELECT COUNT(*) AS n, "
        "COUNT(*) FILTER (WHERE type=2) AS type2, "
        "COUNT(*) FILTER (WHERE type=5) AS type5, "
        "ROUND(AVG(CASE WHEN COALESCE((other::jsonb->>'frt')::bigint, 0) > 0 "
        "THEN (other::jsonb->>'frt')::bigint END)) AS frt_avg, "
        "ROUND(AVG(use_time)) AS use_avg, "
        "ROUND(AVG(CASE WHEN prompt_tokens > 0 THEN "
        "100.0*COALESCE((other::jsonb->>'cache_tokens')::bigint, 0)/prompt_tokens END)) AS cache_pct "
        f"FROM logs WHERE channel_id={channel_id} "
        f"AND created_at >= {since_epoch_s};"
    )
    cmd = [
        "ssh",
        "-o",
        "BatchMode=yes",
        "-o",
        "ConnectTimeout=15",
        ssh_host,
        "docker exec new-api-postgres psql -U newapi -d new-api -At -F'|' -c "
        + shlex.quote(sql.replace("\n", " ")),
    ]
    try:
        proc = subprocess.run(cmd, capture_output=True, text=True, timeout=60, check=False)
    except (subprocess.TimeoutExpired, FileNotFoundError) as exc:
        return {"ok": False, "error": str(exc)}
    line = (proc.stdout or "").strip().splitlines()[0] if proc.stdout else ""
    if proc.returncode != 0 or not line:
        return {
            "ok": False,
            "returncode": proc.returncode,
            "stdout": (proc.stdout or "")[:500],
            "stderr": (proc.stderr or "")[:500],
        }
    parts = line.split("|")
    keys = ["n", "type2", "type5", "frt_avg_ms", "use_avg_s", "cache_pct"]
    return {"ok": True, "channel_id": channel_id, **dict(zip(keys, parts))}


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--pi-bin", default=DEFAULT_PI)
    parser.add_argument("--workers", type=int, default=8)
    parser.add_argument("--target-rpm", type=float, default=40.0)
    parser.add_argument("--duration-minutes", type=float, default=5.0)
    parser.add_argument("--thinking", default=DEFAULT_THINKING)
    parser.add_argument("--channel-id", type=int, default=109)
    parser.add_argument("--ssh-host", default="panda")
    parser.add_argument("--run-dir", default="")
    parser.add_argument("--skip-panda-stats", action="store_true")
    parser.add_argument("--shutdown-grace-s", type=float, default=120.0)
    args = parser.parse_args(argv)

    if args.workers > 8:
        print("workers capped at 8 per user policy", file=sys.stderr)
        args.workers = 8

    stamp = dt.datetime.now().strftime("%Y%m%d-%H%M%S")
    run_dir = Path(args.run_dir) if args.run_dir else RUN_ROOT / f"piagent-rpm-{stamp}"
    run_dir.mkdir(parents=True, exist_ok=True)

    meta = {
        "created_at": dt.datetime.now(dt.timezone.utc).isoformat(),
        "pi_bin": args.pi_bin,
        "workers": args.workers,
        "target_rpm": args.target_rpm,
        "duration_minutes": args.duration_minutes,
        "thinking": args.thinking,
        "route": "closeTest -> NewAPI channel 109 -> panda :4010 -> zen-proxy-test :4011",
        "production_touched": False,
    }
    (run_dir / "meta.json").write_text(json.dumps(meta, indent=2), encoding="utf-8")

    limiter = RateLimiter(args.target_rpm)
    deadline = time.monotonic() + args.duration_minutes * 60.0
    seq = 0
    futures: dict[Any, TaskSpec] = {}
    results: list[TaskResult] = []
    wall_start = time.monotonic()
    summary_note: str | None = None

    print(json.dumps({"event": "start", "run_dir": str(run_dir), **meta}, ensure_ascii=False))

    with ThreadPoolExecutor(max_workers=args.workers) as pool:
        while time.monotonic() < deadline:
            while len(futures) < args.workers and time.monotonic() < deadline:
                limiter.acquire()
                case_id, cwd, prompt, timeout_s = CASES[seq % len(CASES)]
                spec = TaskSpec(seq=seq, case_id=case_id, cwd=cwd, prompt=prompt, timeout_s=timeout_s)
                fut = pool.submit(
                    run_pi_task,
                    spec,
                    pi_bin=args.pi_bin,
                    thinking=args.thinking,
                    run_dir=run_dir,
                )
                futures[fut] = spec
                seq += 1
            if not futures:
                break
            done, _ = wait_any(futures, timeout=1.0)
            for fut in done:
                spec = futures.pop(fut)
                try:
                    row = fut.result()
                except Exception as exc:  # noqa: BLE001
                    row = TaskResult(
                        seq=spec.seq,
                        case_id=spec.case_id,
                        cwd=spec.cwd,
                        started_at=dt.datetime.now(dt.timezone.utc).isoformat(),
                        elapsed_ms=0,
                        exit_code=None,
                        timed_out=False,
                        stdout_sha256=sha256_text(""),
                        stderr_sha256=sha256_text(str(exc)),
                        stdout_bytes=0,
                        stderr_bytes=len(str(exc)),
                        stdout_path="",
                        stderr_path="",
                        json_events=0,
                        assistant_text_chars=0,
                        tool_events=0,
                        subagent_markers=0,
                        error_markers=1,
                        semantic_ok=False,
                    )
                results.append(row)
                print(
                    json.dumps(
                        {
                            "event": "result",
                            "seq": row.seq,
                            "case_id": row.case_id,
                            "exit_code": row.exit_code,
                            "timed_out": row.timed_out,
                            "elapsed_ms": row.elapsed_ms,
                            "semantic_ok": row.semantic_ok,
                            "subagent_markers": row.subagent_markers,
                        },
                        ensure_ascii=False,
                    )
                )

        grace_deadline = time.monotonic() + args.shutdown_grace_s
        while futures and time.monotonic() < grace_deadline:
            done, _ = wait_any(futures, timeout=1.0)
            for fut in done:
                spec = futures.pop(fut)
                try:
                    row = fut.result()
                except Exception as exc:  # noqa: BLE001
                    row = TaskResult(
                        seq=spec.seq,
                        case_id=spec.case_id,
                        cwd=spec.cwd,
                        started_at=dt.datetime.now(dt.timezone.utc).isoformat(),
                        elapsed_ms=0,
                        exit_code=None,
                        timed_out=False,
                        stdout_sha256=sha256_text(""),
                        stderr_sha256=sha256_text(str(exc)),
                        stdout_bytes=0,
                        stderr_bytes=len(str(exc)),
                        stdout_path="",
                        stderr_path="",
                        json_events=0,
                        assistant_text_chars=0,
                        tool_events=0,
                        subagent_markers=0,
                        error_markers=1,
                        semantic_ok=False,
                    )
                results.append(row)
        if futures:
            summary_note = f"{len(futures)} tasks still running after grace; omitted from parent_ok"
        else:
            summary_note = None

    wall_ms = int((time.monotonic() - wall_start) * 1000)
    summary = summarize(results, wall_ms, args.target_rpm)
    if summary_note:
        summary["in_flight_after_grace"] = len(futures)
        summary["note"] = summary_note
    since_epoch_s = int(dt.datetime.fromisoformat(meta["created_at"]).timestamp())
    if not args.skip_panda_stats:
        summary["panda_newapi"] = collect_panda_channel_stats(
            args.channel_id, since_epoch_s, args.ssh_host
        )
    (run_dir / "summary.json").write_text(json.dumps(summary, indent=2), encoding="utf-8")
    with (run_dir / "results.jsonl").open("w", encoding="utf-8") as handle:
        for row in results:
            handle.write(json.dumps(dataclasses.asdict(row), ensure_ascii=False) + "\n")

    print(json.dumps({"event": "done", "run_dir": str(run_dir), "summary": summary}, ensure_ascii=False))
    return 0 if summary["parent_timeouts"] == 0 and summary["semantic_ok"] >= summary["parent_sessions"] * 0.7 else 1


def wait_any(futures: dict[Any, TaskSpec], timeout: float) -> tuple[list[Any], list[Any]]:
    from concurrent.futures import wait

    if not futures:
        return [], []
    done, not_done = wait(list(futures.keys()), timeout=timeout, return_when="FIRST_COMPLETED")
    return list(done), list(not_done)


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
