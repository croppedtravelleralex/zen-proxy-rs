#!/usr/bin/env python3
"""Run the user-defined project matrix through native opencode.

This is the control group for ClaudeCode -> cc-switch -> ZenProxy tests. It
does not touch cc-switch, NewAPI, panda, or local ZenProxy. Results are written
under .local-dev/runs and should not be committed.
"""

from __future__ import annotations

import argparse
import concurrent.futures
import dataclasses
import datetime as dt
import hashlib
import json
import os
import re
import shutil
import subprocess
import time
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parents[2]
LOCAL_DEV = ROOT / ".local-dev"


def resolve_opencode_bin() -> str:
    configured = os.environ.get("OPENCODE_BIN") or shutil.which("opencode") or "opencode"
    path = Path(configured)
    if os.name == "nt" and path.suffix.lower() in {".cmd", ".bat"} and path.exists():
        text = path.read_text(encoding="utf-8", errors="replace")
        match = re.search(r'"([^"]+opencode\.exe)"', text, flags=re.IGNORECASE)
        if match and Path(match.group(1)).exists():
            return match.group(1)
    return configured


OPENCODE_BIN = resolve_opencode_bin()

MODELS = {
    "deepseek-v4-flash": ("opencode/deepseek-v4-flash-free", None),
    "mimo-v2.5": ("opencode/mimo-v2.5-free", None),
    "big-pickle": ("opencode/big-pickle", "max"),
}

CASES = [
    ("tide", r"D:\SelfMadeTool\Tide", "全面深入检查项目并详细汇报。只读检查，不要修改、删除或创建项目文件。"),
    ("mirofish", r"D:\SelfMadeTool\MiroFish", "全面深入检查项目并详细汇报。只读检查，不要修改、删除或创建项目文件。"),
    ("personal-cleanup", r"D:\SelfMadeTool\personal", "检查本机 C、D 盘可删除内容并详细汇报。只读检查，不要删除、移动或修改任何文件。"),
    (
        "outlook-register",
        r"D:\SelfMadeTool\AutoRegister\camoufoxOutlookRegister",
        "全面深入检查项目并详细汇报。只读检查，不要修改、删除或创建项目文件。",
    ),
]


@dataclasses.dataclass
class NativeResult:
    case_id: str
    model: str
    opencode_model: str
    variant: str | None
    cwd: str
    prompt_sha256: str
    exit_code: int | None
    elapsed_ms: int
    timeout: bool
    stdout_bytes: int
    stderr_bytes: int
    stdout_sha256: str
    stderr_sha256: str
    stdout_path: str
    stderr_path: str
    text_chars: int
    event_count: int
    tool_event_count: int
    step_finish_reason: str | None
    input_tokens: int
    output_tokens: int
    reasoning_tokens: int
    cache_read_tokens: int
    cache_write_tokens: int


def utc_run_id() -> str:
    return dt.datetime.now(dt.UTC).strftime("%Y%m%d-%H%M%S")


def sha256_text(value: str) -> str:
    return hashlib.sha256(value.encode("utf-8", errors="replace")).hexdigest()


def local_path(path: str) -> str:
    if os.name == "nt":
        return path
    opencode_path = shutil.which(OPENCODE_BIN) or OPENCODE_BIN
    normalized = opencode_path.replace("\\", "/").lower()
    if normalized.startswith("/mnt/") or normalized.endswith((".exe", ".cmd", ".bat")):
        return path
    if len(path) >= 3 and path[1:3] == ":\\":
        drive = path[0].lower()
        rest = path[3:].replace("\\", "/")
        return f"/mnt/{drive}/{rest}"
    return path


def filesystem_path(path: str) -> Path:
    if os.name != "nt" and len(path) >= 3 and path[1:3] == ":\\":
        drive = path[0].lower()
        rest = path[3:].replace("\\", "/")
        return Path(f"/mnt/{drive}/{rest}")
    return Path(path)


def safe_path_fragment(value: str) -> str:
    return "".join(ch if ch.isalnum() or ch in "-_." else "_" for ch in value)


def isolated_opencode_env(run_dir: Path, public_model: str, case_id: str) -> dict[str, str]:
    env = os.environ.copy()
    if os.name == "nt":
        root = Path(os.environ.get("TEMP") or Path.home() / "AppData" / "Local" / "Temp")
    else:
        root = run_dir / "_opencode-runtime"
    runtime = root / "opencode-matrix" / safe_path_fragment(run_dir.name) / safe_path_fragment(public_model) / safe_path_fragment(case_id)
    data_home = runtime / "data"
    state_home = runtime / "state"
    data_home.mkdir(parents=True, exist_ok=True)
    state_home.mkdir(parents=True, exist_ok=True)
    env["XDG_DATA_HOME"] = str(data_home)
    env["XDG_STATE_HOME"] = str(state_home)
    if "XDG_CONFIG_HOME" not in env:
        env["XDG_CONFIG_HOME"] = str(Path.home() / ".config")
    return env


def run_process(command: list[str], timeout_s: int, cwd: str, env: dict[str, str]) -> tuple[int | None, str, str, bool]:
    creationflags = subprocess.CREATE_NEW_PROCESS_GROUP if os.name == "nt" else 0
    proc = subprocess.Popen(
        command,
        text=True,
        encoding="utf-8",
        errors="replace",
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        cwd=cwd,
        env=env,
        creationflags=creationflags,
    )
    try:
        stdout, stderr = proc.communicate(timeout=timeout_s)
        return proc.returncode, stdout or "", stderr or "", False
    except subprocess.TimeoutExpired:
        if os.name == "nt":
            subprocess.run(
                ["taskkill", "/PID", str(proc.pid), "/T", "/F"],
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                check=False,
            )
        else:
            proc.kill()
        try:
            stdout, stderr = proc.communicate(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()
            stdout, stderr = proc.communicate()
        return None, stdout or "", stderr or "", True


def verify_paths() -> None:
    missing = []
    for case_id, path, _ in CASES:
        if not filesystem_path(path).exists():
            missing.append(f"{case_id}: {path}")
    if missing:
        raise FileNotFoundError("missing project paths:\n" + "\n".join(missing))


def parse_opencode_jsonl(stdout: str) -> dict[str, Any]:
    event_count = 0
    tool_event_count = 0
    text_parts: list[str] = []
    reason = None
    tokens = {
        "input": 0,
        "output": 0,
        "reasoning": 0,
        "cache_read": 0,
        "cache_write": 0,
    }
    for line in stdout.splitlines():
        try:
            event = json.loads(line)
        except json.JSONDecodeError:
            continue
        event_count += 1
        event_type = str(event.get("type") or "")
        part = event.get("part") if isinstance(event.get("part"), dict) else {}
        part_type = str(part.get("type") or "")
        if "tool" in event_type or "tool" in part_type:
            tool_event_count += 1
        if event_type == "text" and isinstance(part.get("text"), str):
            text_parts.append(part["text"])
        if event_type == "step_finish":
            reason = str(part.get("reason") or "") or reason
            step_tokens = part.get("tokens") if isinstance(part.get("tokens"), dict) else {}
            cache = step_tokens.get("cache") if isinstance(step_tokens.get("cache"), dict) else {}
            tokens["input"] += int(step_tokens.get("input") or 0)
            tokens["output"] += int(step_tokens.get("output") or 0)
            tokens["reasoning"] += int(step_tokens.get("reasoning") or 0)
            tokens["cache_read"] += int(cache.get("read") or 0)
            tokens["cache_write"] += int(cache.get("write") or 0)
    return {
        "text": "".join(text_parts),
        "event_count": event_count,
        "tool_event_count": tool_event_count,
        "reason": reason,
        "tokens": tokens,
    }


def run_case(
    run_dir: Path,
    case_id: str,
    cwd: str,
    prompt: str,
    public_model: str,
    opencode_model: str,
    variant: str | None,
    timeout_s: int,
    skip_permissions: bool,
) -> NativeResult:
    case_dir = run_dir / public_model / case_id
    case_dir.mkdir(parents=True, exist_ok=True)
    stdout_path = case_dir / "stdout.jsonl"
    stderr_path = case_dir / "stderr.txt"
    local_cwd = local_path(cwd)
    command = [
        OPENCODE_BIN,
        "run",
        "--format",
        "json",
        "--model",
        opencode_model,
        "--dir",
        local_cwd,
    ]
    if variant:
        command.extend(["--variant", variant])
    if skip_permissions:
        command.append("--dangerously-skip-permissions")
    command.append(prompt)

    started = time.perf_counter()
    exit_code, stdout, stderr, timeout = run_process(
        command,
        timeout_s,
        local_cwd,
        isolated_opencode_env(run_dir, public_model, case_id),
    )

    elapsed_ms = int((time.perf_counter() - started) * 1000)
    stdout_path.write_text(stdout, encoding="utf-8", errors="replace")
    stderr_path.write_text(stderr, encoding="utf-8", errors="replace")
    parsed = parse_opencode_jsonl(stdout)
    tokens = parsed["tokens"]
    return NativeResult(
        case_id=case_id,
        model=public_model,
        opencode_model=opencode_model,
        variant=variant,
        cwd=cwd,
        prompt_sha256=sha256_text(prompt),
        exit_code=exit_code,
        elapsed_ms=elapsed_ms,
        timeout=timeout,
        stdout_bytes=len(stdout.encode("utf-8", errors="replace")),
        stderr_bytes=len(stderr.encode("utf-8", errors="replace")),
        stdout_sha256=sha256_text(stdout),
        stderr_sha256=sha256_text(stderr),
        stdout_path=str(stdout_path),
        stderr_path=str(stderr_path),
        text_chars=len(parsed["text"]),
        event_count=int(parsed["event_count"]),
        tool_event_count=int(parsed["tool_event_count"]),
        step_finish_reason=parsed["reason"],
        input_tokens=int(tokens["input"]),
        output_tokens=int(tokens["output"]),
        reasoning_tokens=int(tokens["reasoning"]),
        cache_read_tokens=int(tokens["cache_read"]),
        cache_write_tokens=int(tokens["cache_write"]),
    )


def write_report(run_dir: Path, results: list[NativeResult]) -> None:
    raw = [dataclasses.asdict(result) for result in results]
    (run_dir / "results.json").write_text(
        json.dumps({"results": raw}, ensure_ascii=False, indent=2) + "\n",
        encoding="utf-8",
    )
    lines = [
        "# opencode native project matrix",
        "",
        f"- run_dir: `{run_dir}`",
        f"- opencode_bin: `{OPENCODE_BIN}`",
        "",
        "| model | opencode_model | case | exit | elapsed_s | events | tool_events | text_chars | input | output | reasoning | cache_read | cache_write | reason |",
        "|---|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---|",
    ]
    for item in results:
        lines.append(
            "| "
            + " | ".join(
                [
                    item.model,
                    item.opencode_model,
                    item.case_id,
                    "timeout" if item.timeout else str(item.exit_code),
                    f"{item.elapsed_ms / 1000:.1f}",
                    str(item.event_count),
                    str(item.tool_event_count),
                    str(item.text_chars),
                    str(item.input_tokens),
                    str(item.output_tokens),
                    str(item.reasoning_tokens),
                    str(item.cache_read_tokens),
                    str(item.cache_write_tokens),
                    item.step_finish_reason or "",
                ]
            )
            + " |"
        )
    (run_dir / "summary.md").write_text("\n".join(lines) + "\n", encoding="utf-8")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--run-id", default=utc_run_id())
    parser.add_argument("--timeout-s", type=int, default=1800)
    parser.add_argument("--models", nargs="+", default=list(MODELS))
    parser.add_argument("--cases", nargs="+", default=[case_id for case_id, _, _ in CASES])
    parser.add_argument("--smoke", action="store_true")
    parser.add_argument(
        "--no-skip-permissions",
        "--no-auto",
        action="store_true",
        help="do not auto-approve opencode tool permissions; --no-auto is kept as a legacy alias",
    )
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    verify_paths()
    selected_models = [model for model in args.models if model in MODELS]
    selected_cases = [case for case in CASES if case[0] in set(args.cases)]
    if args.smoke:
        selected_models = selected_models[:1]
        selected_cases = selected_cases[:1]
    if not selected_models or not selected_cases:
        raise SystemExit("no selected models/cases")

    run_dir = LOCAL_DEV / "runs" / f"opencode-native-project-matrix-{args.run_id}"
    run_dir.mkdir(parents=True, exist_ok=True)
    results: list[NativeResult] = []
    for public_model in selected_models:
        opencode_model, variant = MODELS[public_model]
        print(
            f"=== run native model={public_model} opencode={opencode_model} cases={len(selected_cases)} parallel ===",
            flush=True,
        )
        with concurrent.futures.ThreadPoolExecutor(max_workers=len(selected_cases)) as pool:
            futures = [
                pool.submit(
                    run_case,
                    run_dir,
                    case_id,
                    cwd,
                    prompt,
                    public_model,
                    opencode_model,
                    variant,
                    args.timeout_s,
                    not args.no_skip_permissions,
                )
                for case_id, cwd, prompt in selected_cases
            ]
            for fut in concurrent.futures.as_completed(futures):
                result = fut.result()
                results.append(result)
                write_report(run_dir, results)
                print(
                    f"done model={public_model} case={result.case_id} exit={result.exit_code} "
                    f"elapsed={result.elapsed_ms / 1000:.1f}s text_chars={result.text_chars}",
                    flush=True,
                )
            print(
                f"=== done native model={public_model} cases={len(selected_cases)} ===",
                flush=True,
            )
    write_report(run_dir, results)
    print(f"summary: {run_dir / 'summary.md'}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
