#!/usr/bin/env python3
"""Run the production NewAPI ClaudeCode project matrix on Windows and WSL.

For each model, this runner starts the four project prompts concurrently on
Windows ClaudeCode and WSL ClaudeCode, then moves to the next model. It reads
the NewAPI base URL and API key from existing cc-switch Claude providers, but
never writes the key into result files.
"""

from __future__ import annotations

import argparse
import collections
import concurrent.futures
import dataclasses
import datetime as dt
import hashlib
import json
import os
import shlex
import sqlite3
import subprocess
import sys
import time
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parents[2]
RUN_ROOT = ROOT / ".local-dev" / "runs"
CCS_DB = Path("/mnt/c/Users/Lenovo/.cc-switch/cc-switch.db")
WINDOWS_CLAUDE = os.environ.get("WINDOWS_CLAUDE_BIN", r"C:\software\nodejs\node_global\claude.cmd")
WSL_CLAUDE = os.environ.get("PANDA_WSL_CLAUDE_BIN", "/home/lenovo/.local/bin/claude")
AUDIT_REMOTE = f"/var/log/zen-proxy-rs/audit/requests-{dt.datetime.now().strftime('%Y-%m-%d')}.jsonl"

PROVIDERS = {
    "deepseek-v4-flash": "codex-closeapi-deepseek",
    "mimo-v2.5": "codex-closeapi-mimo",
    "big-pickle": "codex-closeapi-bigpickle",
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
class ProviderConfig:
    provider_id: str
    base_url: str
    api_key: str


@dataclasses.dataclass
class CaseResult:
    model: str
    platform: str
    case_id: str
    cwd: str
    exit_code: int | None
    timeout: bool
    elapsed_ms: int
    stdout_bytes: int
    stderr_bytes: int
    stdout_sha256: str
    stderr_sha256: str
    stdout_path: str
    stderr_path: str


def sha256_text(value: str) -> str:
    return hashlib.sha256(value.encode("utf-8", errors="replace")).hexdigest()


def ps_quote(value: str) -> str:
    return "'" + value.replace("'", "''") + "'"


def wsl_path(win_path: str) -> str:
    proc = subprocess.run(
        ["wslpath", "-u", win_path],
        text=True,
        capture_output=True,
        check=True,
    )
    return proc.stdout.strip()


def windows_path_from_wsl(path: Path) -> str:
    proc = subprocess.run(
        ["wslpath", "-w", str(path)],
        text=True,
        capture_output=True,
        check=True,
    )
    return proc.stdout.strip()


def windows_path_exists(path: str) -> bool:
    proc = subprocess.run(
        [
            "powershell.exe",
            "-NoProfile",
            "-Command",
            f"if (Test-Path -LiteralPath {ps_quote(path)}) {{ exit 0 }} else {{ exit 3 }}",
        ],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        check=False,
    )
    return proc.returncode == 0


def load_provider(model: str) -> ProviderConfig:
    provider_id = PROVIDERS[model]
    with sqlite3.connect(CCS_DB) as conn:
        conn.row_factory = sqlite3.Row
        row = conn.execute(
            "select settings_config from providers where app_type='claude' and id=?",
            (provider_id,),
        ).fetchone()
    if not row:
        raise RuntimeError(f"missing cc-switch provider: {provider_id}")
    settings = json.loads(row["settings_config"] or "{}")
    env = settings.get("env") if isinstance(settings, dict) else {}
    base_url = env.get("ANTHROPIC_BASE_URL") or settings.get("base_url")
    api_key = env.get("ANTHROPIC_AUTH_TOKEN") or env.get("ANTHROPIC_API_KEY") or settings.get("api_key")
    if not base_url or not api_key:
        raise RuntimeError(f"provider {provider_id} lacks base URL or API key")
    return ProviderConfig(provider_id=provider_id, base_url=str(base_url).rstrip("/"), api_key=str(api_key))


def verify_inputs() -> None:
    if not CCS_DB.exists():
        raise FileNotFoundError(CCS_DB)
    missing = [f"{case_id}: {path}" for case_id, path, _ in CASES if not windows_path_exists(path)]
    if missing:
        raise FileNotFoundError("missing Windows project paths:\n" + "\n".join(missing))
    if not windows_path_exists(WINDOWS_CLAUDE):
        raise FileNotFoundError(f"missing Windows ClaudeCode binary: {WINDOWS_CLAUDE}")
    if not Path(WSL_CLAUDE).exists():
        raise FileNotFoundError(f"missing WSL ClaudeCode binary: {WSL_CLAUDE}")


def audit_offset() -> int:
    cmd = f"test -f {shlex.quote(AUDIT_REMOTE)} && stat -c %s {shlex.quote(AUDIT_REMOTE)} || echo 0"
    proc = subprocess.run(["ssh", "panda", cmd], text=True, capture_output=True, check=True)
    return int((proc.stdout.strip() or "0").splitlines()[-1])


def read_audit_since(offset: int) -> tuple[list[dict[str, Any]], str | None]:
    start = offset + 1
    cmd = (
        f"test -f {shlex.quote(AUDIT_REMOTE)} && "
        f"tail -c +{start} {shlex.quote(AUDIT_REMOTE)} || true"
    )
    try:
        proc = subprocess.run(
            ["ssh", "-o", "ConnectTimeout=10", "panda", cmd],
            text=True,
            capture_output=True,
            timeout=60,
            check=False,
        )
    except subprocess.TimeoutExpired:
        return [], "ssh_timeout"
    if proc.returncode != 0:
        stderr = (proc.stderr or "").strip().replace("\n", " ")
        return [], f"ssh_exit_{proc.returncode}: {stderr[:400]}"
    rows: list[dict[str, Any]] = []
    for line in proc.stdout.splitlines():
        try:
            row = json.loads(line)
        except json.JSONDecodeError:
            continue
        rows.append(row)
    return rows, None


def audit_model_matches(row: dict[str, Any], model: str) -> bool:
    return (row.get("public_model") or row.get("model")) == model


def audit_model_triplets(rows: list[dict[str, Any]]) -> list[dict[str, Any]]:
    counts: collections.Counter[tuple[str, str, str]] = collections.Counter()
    for row in rows:
        counts[
            (
                str(row.get("public_model") or ""),
                str(row.get("model") or ""),
                str(row.get("upstream_model") or ""),
            )
        ] += 1
    return [
        {"public_model": public, "model": model, "upstream_model": upstream, "rows": count}
        for (public, model, upstream), count in counts.most_common()
    ]


def audit_summary(rows: list[dict[str, Any]]) -> dict[str, Any]:
    read = 0
    miss = 0
    prompt = 0
    completion = 0
    ttfts: list[int] = []
    outcomes: dict[str, int] = {}
    status_outcomes: collections.Counter[str] = collections.Counter()
    warmup_states: collections.Counter[str] = collections.Counter()
    session_pin_hits: collections.Counter[str] = collections.Counter()
    prefix_hashes: collections.Counter[str] = collections.Counter()
    for row in rows:
        usage = row.get("usage") if isinstance(row.get("usage"), dict) else row
        cr = int(usage.get("cache_read_input_tokens") or usage.get("cached_tokens") or 0)
        cm_raw = usage.get("cache_miss_input_tokens")
        prompt_tokens = int(usage.get("prompt_tokens") or row.get("prompt_tokens") or 0)
        if cm_raw is None:
            cm = max(prompt_tokens - cr, 0)
        else:
            cm = int(cm_raw or 0)
        read += cr
        miss += cm
        prompt += prompt_tokens
        completion += int(usage.get("completion_tokens") or row.get("completion_tokens") or 0)
        outcome = str(row.get("outcome") or "unknown")
        outcomes[outcome] = outcomes.get(outcome, 0) + 1
        status_outcomes[f"{row.get('status')}:{outcome}:{row.get('failure_kind') or ''}"] += 1
        warmup_states[str(row.get("warmup_state"))] += 1
        session_pin_hits[str(row.get("session_pin_hit"))] += 1
        if row.get("prefix_32k_hash"):
            prefix_hashes[str(row["prefix_32k_hash"])] += 1
        timings = row.get("timings") if isinstance(row.get("timings"), dict) else {}
        ttft = int(row.get("ttft_ms") or timings.get("protocol_first_byte_ms") or timings.get("first_content_token_ms") or 0)
        if ttft > 0:
            ttfts.append(ttft)
    ttfts.sort()
    denominator = read + miss
    return {
        "rows": len(rows),
        "read_tokens": read,
        "miss_tokens": miss,
        "prompt_tokens": prompt,
        "completion_tokens": completion,
        "r2_pct": round(read / denominator * 100.0, 2) if denominator else None,
        "outcomes": outcomes,
        "status_outcomes": dict(status_outcomes.most_common()),
        "ttft_p50_ms": ttfts[len(ttfts) // 2] if ttfts else None,
        "ttft_p90_ms": ttfts[min(len(ttfts) - 1, round((len(ttfts) - 1) * 0.9))] if ttfts else None,
        "warmup_state": dict(warmup_states.most_common()),
        "session_pin_hit": dict(session_pin_hits.most_common()),
        "unique_prefix_32k_hashes": len(prefix_hashes),
        "reused_prefix_32k_hashes": [
            {"hash": prefix, "rows": count}
            for prefix, count in prefix_hashes.most_common(10)
            if count > 1
        ],
        "model_triplets": audit_model_triplets(rows),
    }


def run_windows_case(
    model: str,
    cfg: ProviderConfig,
    case_id: str,
    cwd: str,
    prompt: str,
    case_dir: Path,
    timeout_s: int,
) -> CaseResult:
    case_dir.mkdir(parents=True, exist_ok=True)
    prompt_path = case_dir / "prompt.txt"
    stdout_path = case_dir / "stdout.txt"
    stderr_path = case_dir / "stderr.txt"
    prompt_path.write_text(prompt, encoding="utf-8")
    prompt_path_win = windows_path_from_wsl(prompt_path)
    env = os.environ.copy()
    env.update(
        {
            "ANTHROPIC_BASE_URL": cfg.base_url,
            "ANTHROPIC_AUTH_TOKEN": cfg.api_key,
            "ANTHROPIC_API_KEY": cfg.api_key,
            "ANTHROPIC_MODEL": model,
            "ANTHROPIC_SMALL_FAST_MODEL": model,
            "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC": "1",
        }
    )
    ps = "\n".join(
        [
            "$ErrorActionPreference = 'Continue'",
            "[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false)",
            f"Set-Location -LiteralPath {ps_quote(cwd)}",
            f"$prompt = Get-Content -Raw -LiteralPath {ps_quote(prompt_path_win)}",
            f"& {ps_quote(WINDOWS_CLAUDE)} -p $prompt --model {ps_quote(model)} --output-format stream-json --verbose --permission-mode bypassPermissions --no-session-persistence --add-dir {ps_quote(cwd)}",
            "exit $LASTEXITCODE",
        ]
    )
    started = time.perf_counter()
    timed_out = False
    try:
        proc = subprocess.run(
            ["powershell.exe", "-NoProfile", "-ExecutionPolicy", "Bypass", "-Command", ps],
            text=True,
            encoding="utf-8",
            errors="replace",
            capture_output=True,
            timeout=timeout_s,
            check=False,
            env=env,
        )
        code = proc.returncode
        stdout = proc.stdout or ""
        stderr = proc.stderr or ""
    except subprocess.TimeoutExpired as exc:
        timed_out = True
        code = None
        stdout = (exc.stdout or "").decode("utf-8", errors="replace") if isinstance(exc.stdout, bytes) else (exc.stdout or "")
        stderr = (exc.stderr or "").decode("utf-8", errors="replace") if isinstance(exc.stderr, bytes) else (exc.stderr or "")
    elapsed = int((time.perf_counter() - started) * 1000)
    stdout_path.write_text(stdout, encoding="utf-8", errors="replace")
    stderr_path.write_text(stderr, encoding="utf-8", errors="replace")
    return CaseResult(model, "windows", case_id, cwd, code, timed_out, elapsed, len(stdout.encode()), len(stderr.encode()), sha256_text(stdout), sha256_text(stderr), str(stdout_path), str(stderr_path))


def run_wsl_case(
    model: str,
    cfg: ProviderConfig,
    case_id: str,
    cwd: str,
    prompt: str,
    case_dir: Path,
    timeout_s: int,
) -> CaseResult:
    case_dir.mkdir(parents=True, exist_ok=True)
    prompt_path = case_dir / "prompt.txt"
    stdout_path = case_dir / "stdout.txt"
    stderr_path = case_dir / "stderr.txt"
    claude_config_dir = case_dir / "_claude-config"
    claude_config_dir.mkdir(parents=True, exist_ok=True)
    prompt_path.write_text(prompt, encoding="utf-8")
    cwd_wsl = wsl_path(cwd)
    env = os.environ.copy()
    env.update(
        {
            "ANTHROPIC_BASE_URL": cfg.base_url,
            "ANTHROPIC_AUTH_TOKEN": cfg.api_key,
            "ANTHROPIC_API_KEY": cfg.api_key,
            "ANTHROPIC_MODEL": model,
            "ANTHROPIC_SMALL_FAST_MODEL": model,
            "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC": "1",
            # WSL user settings may point at a stale local gateway. Keep the
            # matrix hermetic so process env wins without writing secrets.
            "CLAUDE_CONFIG_DIR": str(claude_config_dir),
        }
    )
    script = (
        f"cd {shlex.quote(cwd_wsl)} && "
        f"cat {shlex.quote(str(prompt_path))} | {shlex.quote(WSL_CLAUDE)} -p "
        f"--model {shlex.quote(model)} --output-format stream-json --verbose "
        f"--permission-mode bypassPermissions --no-session-persistence "
        f"--setting-sources user --add-dir {shlex.quote(cwd_wsl)}"
    )
    started = time.perf_counter()
    timed_out = False
    try:
        proc = subprocess.run(
            ["timeout", f"{timeout_s}s", "bash", "-lc", script],
            text=True,
            encoding="utf-8",
            errors="replace",
            capture_output=True,
            timeout=timeout_s + 5,
            check=False,
            env=env,
        )
        code = proc.returncode
        stdout = proc.stdout or ""
        stderr = proc.stderr or ""
        timed_out = code == 124
    except subprocess.TimeoutExpired as exc:
        timed_out = True
        code = None
        stdout = (exc.stdout or "").decode("utf-8", errors="replace") if isinstance(exc.stdout, bytes) else (exc.stdout or "")
        stderr = (exc.stderr or "").decode("utf-8", errors="replace") if isinstance(exc.stderr, bytes) else (exc.stderr or "")
    elapsed = int((time.perf_counter() - started) * 1000)
    stdout_path.write_text(stdout, encoding="utf-8", errors="replace")
    stderr_path.write_text(stderr, encoding="utf-8", errors="replace")
    return CaseResult(model, "wsl", case_id, cwd_wsl, code, timed_out, elapsed, len(stdout.encode()), len(stderr.encode()), sha256_text(stdout), sha256_text(stderr), str(stdout_path), str(stderr_path))


def run_model(model: str, timeout_s: int, run_dir: Path) -> dict[str, Any]:
    cfg = load_provider(model)
    model_dir = run_dir / model
    model_dir.mkdir(parents=True, exist_ok=True)
    start_offset = audit_offset()
    started = time.time()
    futures: list[concurrent.futures.Future[CaseResult]] = []
    results: list[CaseResult] = []
    with concurrent.futures.ThreadPoolExecutor(max_workers=8) as pool:
        for case_id, cwd, prompt in CASES:
            futures.append(
                pool.submit(
                    run_windows_case,
                    model,
                    cfg,
                    case_id,
                    cwd,
                    prompt,
                    model_dir / "windows" / case_id,
                    timeout_s,
                )
            )
            futures.append(
                pool.submit(
                    run_wsl_case,
                    model,
                    cfg,
                    case_id,
                    cwd,
                    prompt,
                    model_dir / "wsl" / case_id,
                    timeout_s,
                )
            )
        for fut in concurrent.futures.as_completed(futures):
            result = fut.result()
            results.append(result)
            print(
                json.dumps(
                    {
                        "event": "case_done",
                        "model": result.model,
                        "platform": result.platform,
                        "case": result.case_id,
                        "exit": "timeout" if result.timeout else result.exit_code,
                        "elapsed_s": round(result.elapsed_ms / 1000, 1),
                    },
                    ensure_ascii=False,
                ),
                flush=True,
            )
    time.sleep(3)
    audit_rows_window, audit_read_error = read_audit_since(start_offset)
    audit_rows_exact = [row for row in audit_rows_window if audit_model_matches(row, model)]
    audit_rows = audit_rows_exact or audit_rows_window
    audit_selection = "exact_model" if audit_rows_exact else "time_window_fallback"
    route_mismatch = any(not audit_model_matches(row, model) for row in audit_rows_window)
    summary = {
        "model": model,
        "provider_id": cfg.provider_id,
        "base_url": cfg.base_url,
        "started_at": started,
        "elapsed_s": round(time.time() - started, 1),
        "results": [dataclasses.asdict(item) for item in sorted(results, key=lambda x: (x.platform, x.case_id))],
        "audit": audit_summary(audit_rows),
        "audit_selection": audit_selection,
        "audit_exact_model": audit_summary(audit_rows_exact),
        "audit_time_window": audit_summary(audit_rows_window),
        "audit_read_error": audit_read_error,
        "audit_route_mismatch": route_mismatch,
    }
    (model_dir / "summary.json").write_text(json.dumps(summary, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    return summary


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--models", nargs="+", default=list(PROVIDERS))
    parser.add_argument("--timeout-s", type=int, default=1800)
    parser.add_argument("--run-id", default=dt.datetime.now(dt.UTC).strftime("%Y%m%d-%H%M%S"))
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    verify_inputs()
    run_dir = RUN_ROOT / f"newapi-dualhost-project-matrix-{args.run_id}"
    run_dir.mkdir(parents=True, exist_ok=True)
    all_summaries = []
    for model in args.models:
        if model not in PROVIDERS:
            raise SystemExit(f"unsupported model: {model}")
        print(json.dumps({"event": "model_start", "model": model}, ensure_ascii=False), flush=True)
        summary = run_model(model, args.timeout_s, run_dir)
        all_summaries.append(summary)
        print(json.dumps({"event": "model_done", "model": model, "audit": summary["audit"]}, ensure_ascii=False), flush=True)
    (run_dir / "summary.json").write_text(json.dumps(all_summaries, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print(json.dumps({"event": "done", "run_dir": str(run_dir)}, ensure_ascii=False), flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
