#!/usr/bin/env python3
"""Run the real Windows ClaudeCode project matrix through cc-switch.

This runner intentionally drives the same local proxy path as manual Windows
ClaudeCode sessions:

ClaudeCode -> cc-switch :15721 -> selected local-zen-* provider -> zen-proxy :14000.

It backs up cc-switch settings/SQLite, switches the Claude provider per model,
executes the four user-defined project prompts, summarizes cc-switch DB rows and
local ZenProxy audit rows, and restores the original provider on exit.
"""

from __future__ import annotations

import argparse
import contextlib
import dataclasses
import datetime as dt
import hashlib
import json
import os
import shutil
import sqlite3
import subprocess
import sys
import time
import urllib.request
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parents[2]
LOCAL_DEV = ROOT / ".local-dev"
CCS_HOME_WIN = os.environ.get("CCSWITCH_HOME", r"C:\Users\Lenovo\.cc-switch")
def windows_file_path(path: str) -> Path:
    if os.name == "nt":
        return Path(path)
    if len(path) >= 3 and path[1:3] == ":\\":
        drive = path[0].lower()
        rest = path[3:].replace("\\", "/")
        return Path(f"/mnt/{drive}/{rest}")
    return Path(path)


CCS_HOME = windows_file_path(CCS_HOME_WIN)
CCS_SETTINGS = CCS_HOME / "settings.json"
CCS_DB = CCS_HOME / "cc-switch.db"
CLAUDE_BIN = os.environ.get("WINDOWS_CLAUDE_BIN", r"C:\software\nodejs\node_global\claude.cmd")
CCS_EXE = os.environ.get("CCSWITCH_EXE", r"D:\SelfMadeTool\CCSwitch\cc-switch.exe")
ZEN_BASE_URL = os.environ.get("LOCAL_ZEN_BASE_URL", "http://127.0.0.1:14000")
CCS_HEALTH_URL = os.environ.get("CCSWITCH_HEALTH_URL", "http://127.0.0.1:15721/health")
LOCAL_PROXY_API_KEY = os.environ.get("LOCAL_PROXY_API_KEY")

PROVIDERS = {
    "deepseek-v4-flash": "local-zen-deepseek",
    "mimo-v2.5": "local-zen-mimo",
    "big-pickle": "local-zen-bigpickle",
}

CASES = [
    (
        "tide",
        r"D:\SelfMadeTool\Tide",
        "全面深入检查项目并详细汇报",
    ),
    (
        "mirofish",
        r"D:\SelfMadeTool\MiroFish",
        "全面深入检查项目并详细汇报",
    ),
    (
        "personal-cleanup",
        r"D:\SelfMadeTool\personal",
        "检查本机c、d盘可删除内容",
    ),
    (
        "outlook-register",
        r"D:\SelfMadeTool\AutoRegister\camoufoxOutlookRegister",
        "全面深入检查项目并详细汇报",
    ),
]


@dataclasses.dataclass
class RunResult:
    case_id: str
    model: str
    provider_id: str
    cwd: str
    prompt_sha256: str
    exit_code: int | None
    elapsed_ms: int
    stdout_bytes: int
    stderr_bytes: int
    stdout_sha256: str
    stderr_sha256: str
    stdout_path: str
    stderr_path: str
    timeout: bool
    ccs_rows: int = 0
    ccs_ok_rows: int = 0
    ccs_error_rows: int = 0
    ccs_input_tokens: int = 0
    ccs_cache_read_tokens: int = 0
    ccs_cache_creation_tokens: int = 0
    ccs_latency_ms_p50: int | None = None
    ccs_first_token_ms_p50: int | None = None
    audit_rows: int = 0
    audit_ok_rows: int = 0
    audit_error_rows: int = 0
    audit_read_tokens: int = 0
    audit_miss_tokens: int = 0
    audit_r2_pct: float | None = None
    audit_ttft_ms_p50: int | None = None
    audit_ttft_ms_p90: int | None = None


def utc_run_id() -> str:
    return dt.datetime.now(dt.UTC).strftime("%Y%m%d-%H%M%S")


def sha256_text(value: str) -> str:
    return hashlib.sha256(value.encode("utf-8", errors="replace")).hexdigest()


def sha256_bytes(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def percentile(values: list[int], pct: int) -> int | None:
    if not values:
        return None
    values = sorted(values)
    index = round((pct / 100) * (len(values) - 1))
    return values[max(0, min(index, len(values) - 1))]


def load_settings() -> dict[str, Any]:
    return json.loads(CCS_SETTINGS.read_text(encoding="utf-8"))


def save_settings(settings: dict[str, Any]) -> None:
    CCS_SETTINGS.write_text(
        json.dumps(settings, ensure_ascii=False, indent=2) + "\n",
        encoding="utf-8",
    )


def sqlite_connect() -> sqlite3.Connection:
    conn = sqlite3.connect(str(CCS_DB), timeout=30)
    conn.row_factory = sqlite3.Row
    return conn


def backup_ccswitch(run_dir: Path) -> dict[str, str]:
    backup_dir = run_dir / "ccswitch-backup"
    backup_dir.mkdir(parents=True, exist_ok=True)
    settings_backup = backup_dir / "settings.json"
    db_backup = backup_dir / "cc-switch.db"
    shutil.copy2(CCS_SETTINGS, settings_backup)
    with sqlite_connect() as src, sqlite3.connect(str(db_backup)) as dst:
        src.backup(dst)
    return {"settings": str(settings_backup), "db": str(db_backup)}


def get_current_provider() -> str:
    return str(load_settings().get("currentProviderClaude") or "")


def switch_provider(provider_id: str) -> None:
    ensure_local_provider_config(provider_id)
    settings = load_settings()
    settings["currentProviderClaude"] = provider_id
    save_settings(settings)
    with sqlite_connect() as conn:
        conn.execute("update providers set is_current=0 where app_type='claude'")
        updated = conn.execute(
            "update providers set is_current=1 where app_type='claude' and id=?",
            (provider_id,),
        ).rowcount
        if updated != 1:
            raise RuntimeError(f"provider not found or not unique: {provider_id}")
        conn.commit()


def ensure_local_provider_config(provider_id: str) -> None:
    model = next((name for name, pid in PROVIDERS.items() if pid == provider_id), None)
    if model is None:
        return
    auth_token = local_proxy_api_key()
    config = {
        "env": {
            "ANTHROPIC_AUTH_TOKEN": auth_token,
            "ANTHROPIC_BASE_URL": ZEN_BASE_URL,
            "ANTHROPIC_MODEL": model,
            "ANTHROPIC_DEFAULT_HAIKU_MODEL": model,
            "ANTHROPIC_DEFAULT_SONNET_MODEL": f"{model}[1M]",
            "ANTHROPIC_DEFAULT_OPUS_MODEL": f"{model}[1M]",
            "ANTHROPIC_DEFAULT_FABLE_MODEL_NAME": model,
            "ANTHROPIC_DEFAULT_HAIKU_MODEL_NAME": model,
            "ANTHROPIC_DEFAULT_FABLE_MODEL": f"{model}[1M]",
            "ANTHROPIC_DEFAULT_SONNET_MODEL_NAME": model,
            "ANTHROPIC_DEFAULT_OPUS_MODEL_NAME": model,
        },
        "model": "opus",
    }
    with sqlite_connect() as conn:
        updated = conn.execute(
            "update providers set settings_config=? where app_type='claude' and id=?",
            (json.dumps(config, ensure_ascii=False, separators=(",", ":")), provider_id),
        ).rowcount
        if updated != 1:
            raise RuntimeError(f"provider not found or not unique: {provider_id}")
        conn.commit()


def local_proxy_api_key() -> str:
    if LOCAL_PROXY_API_KEY:
        return LOCAL_PROXY_API_KEY
    with sqlite_connect() as conn:
        row = conn.execute(
            "select settings_config from providers where app_type='claude' and id=?",
            ("codex-closeapi-bigpickle",),
        ).fetchone()
    if row:
        with contextlib.suppress(Exception):
            token = json.loads(row["settings_config"])["env"]["ANTHROPIC_AUTH_TOKEN"]
            if isinstance(token, str) and token.strip():
                return token
    return "local-dev-proxy"


def restart_ccswitch() -> None:
    ps = "\n".join(
        [
            "$ErrorActionPreference = 'Stop'",
            "Get-Process -Name 'cc-switch' -ErrorAction SilentlyContinue | Stop-Process -Force",
            "Start-Sleep -Milliseconds 800",
            f"Start-Process -FilePath {ps_quote(CCS_EXE)} -WindowStyle Hidden",
        ]
    )
    subprocess.run(
        ["powershell.exe", "-NoProfile", "-ExecutionPolicy", "Bypass", "-Command", ps],
        check=True,
    )
    deadline = time.time() + 30
    last_error = ""
    while time.time() < deadline:
        try:
            with urllib.request.urlopen(CCS_HEALTH_URL, timeout=2) as response:
                if response.status == 200:
                    return
        except Exception as exc:  # noqa: BLE001 - health loop reports final error.
            last_error = str(exc)
        time.sleep(1)
    raise RuntimeError(f"cc-switch did not become healthy: {last_error}")


def healthcheck_local_zen() -> None:
    with urllib.request.urlopen(f"{ZEN_BASE_URL}/health", timeout=5) as response:
        if response.status != 200:
            raise RuntimeError(f"local zen health status={response.status}")


def verify_paths() -> None:
    missing = []
    for label, path, _ in CASES:
        if not windows_path_exists(path):
            missing.append(f"{label}: {path}")
    if missing:
        raise FileNotFoundError("missing project paths:\n" + "\n".join(missing))
    if not windows_path_exists(CLAUDE_BIN):
        raise FileNotFoundError(f"missing ClaudeCode binary: {CLAUDE_BIN}")
    if not CCS_SETTINGS.exists() or not CCS_DB.exists():
        raise FileNotFoundError(f"missing cc-switch settings/db under {CCS_HOME}")


def ps_quote(value: str) -> str:
    return "'" + value.replace("'", "''") + "'"


def windows_path_exists(path: str) -> bool:
    if os.name == "nt":
        return Path(path).exists()
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


def coerce_text(value: Any) -> str:
    if value is None:
        return ""
    if isinstance(value, bytes):
        return value.decode("utf-8", errors="replace")
    return str(value)


def run_claude_windows(command_args: list[str], cwd: str, timeout_s: int) -> subprocess.CompletedProcess[str]:
    if os.name == "nt":
        return subprocess.run(
            command_args,
            cwd=cwd,
            text=True,
            encoding="utf-8",
            errors="replace",
            capture_output=True,
            timeout=timeout_s,
            check=False,
        )
    ps_lines = [
        "$ErrorActionPreference = 'Stop'",
        "[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false)",
        f"Set-Location -LiteralPath {ps_quote(cwd)}",
        f"$claude = {ps_quote(CLAUDE_BIN)}",
        "$claudeArgs = @()",
    ]
    for arg in command_args[1:]:
        ps_lines.append(f"$claudeArgs += {ps_quote(arg)}")
    ps_lines.extend(["& $claude @claudeArgs", "exit $LASTEXITCODE"])
    return subprocess.run(
        ["powershell.exe", "-NoProfile", "-ExecutionPolicy", "Bypass", "-Command", "\n".join(ps_lines)],
        text=True,
        encoding="utf-8",
        errors="replace",
        capture_output=True,
        timeout=timeout_s,
        check=False,
    )


def ccs_stats(provider_id: str, created_after_s: int) -> dict[str, Any]:
    with sqlite_connect() as conn:
        rows = conn.execute(
            """
            select *
            from proxy_request_logs
            where app_type='claude'
              and provider_id=?
              and created_at>=?
            order by created_at asc
            """,
            (provider_id, created_after_s),
        ).fetchall()
    latencies = [int(row["latency_ms"] or 0) for row in rows if int(row["latency_ms"] or 0) > 0]
    first_tokens = [
        int(row["first_token_ms"] or 0)
        for row in rows
        if int(row["first_token_ms"] or 0) > 0
    ]
    return {
        "rows": len(rows),
        "ok_rows": sum(1 for row in rows if int(row["status_code"] or 0) < 400),
        "error_rows": sum(1 for row in rows if int(row["status_code"] or 0) >= 400),
        "input_tokens": sum(int(row["input_tokens"] or 0) for row in rows),
        "cache_read_tokens": sum(int(row["cache_read_tokens"] or 0) for row in rows),
        "cache_creation_tokens": sum(int(row["cache_creation_tokens"] or 0) for row in rows),
        "latency_ms_p50": percentile(latencies, 50),
        "first_token_ms_p50": percentile(first_tokens, 50),
        "errors": [
            str(row["error_message"] or "")[:300]
            for row in rows
            if int(row["status_code"] or 0) >= 400 or row["error_message"]
        ][:5],
    }


def audit_path() -> Path:
    return LOCAL_DEV / "audit" / f"requests-{dt.datetime.now().strftime('%Y-%m-%d')}.jsonl"


def read_audit_rows(start_offset: int, model: str) -> list[dict[str, Any]]:
    path = audit_path()
    if not path.exists():
        return []
    with path.open("rb") as handle:
        handle.seek(start_offset)
        data = handle.read().decode("utf-8", errors="replace")
    rows = []
    for line in data.splitlines():
        try:
            row = json.loads(line)
        except json.JSONDecodeError:
            continue
        if (row.get("public_model") or row.get("model")) == model:
            rows.append(row)
    return rows


def audit_stats(rows: list[dict[str, Any]]) -> dict[str, Any]:
    ttfts = []
    read = 0
    miss = 0
    ok = 0
    for row in rows:
        usage = row.get("usage") if isinstance(row.get("usage"), dict) else row
        cr = int(usage.get("cache_read_input_tokens") or usage.get("cached_tokens") or 0)
        cm = usage.get("cache_miss_input_tokens")
        if cm is None:
            prompt = int(usage.get("prompt_tokens") or row.get("prompt_tokens") or 0)
            cm = max(prompt - cr, 0)
        cm = int(cm or 0)
        read += cr
        miss += cm
        if row.get("outcome") == "success":
            ok += 1
        timings = row.get("timings") if isinstance(row.get("timings"), dict) else {}
        ttft = int(
            row.get("ttft_ms")
            or timings.get("protocol_first_byte_ms")
            or timings.get("first_content_token_ms")
            or 0
        )
        if ttft > 0:
            ttfts.append(ttft)
    denominator = read + miss
    return {
        "rows": len(rows),
        "ok_rows": ok,
        "error_rows": len(rows) - ok,
        "read_tokens": read,
        "miss_tokens": miss,
        "r2_pct": round(read / denominator * 100.0, 2) if denominator else None,
        "ttft_ms_p50": percentile(ttfts, 50),
        "ttft_ms_p90": percentile(ttfts, 90),
        "outcomes": sorted({str(row.get("outcome") or "") for row in rows if row.get("outcome")}),
    }


def run_case(
    run_dir: Path,
    case_id: str,
    cwd: str,
    prompt: str,
    model: str,
    provider_id: str,
    timeout_s: int,
) -> RunResult:
    case_dir = run_dir / model / case_id
    case_dir.mkdir(parents=True, exist_ok=True)
    stdout_path = case_dir / "stdout.txt"
    stderr_path = case_dir / "stderr.txt"
    created_after_s = int(time.time()) - 2
    audit_file = audit_path()
    audit_start = audit_file.stat().st_size if audit_file.exists() else 0
    command = [
        CLAUDE_BIN,
        "-p",
        prompt,
        "--model",
        model,
        "--output-format",
        "stream-json",
        "--verbose",
        "--permission-mode",
        "bypassPermissions",
        "--no-session-persistence",
        "--add-dir",
        str(cwd),
    ]
    started = time.perf_counter()
    timeout = False
    try:
        proc = run_claude_windows(command, cwd, timeout_s)
        exit_code = proc.returncode
        stdout = coerce_text(proc.stdout)
        stderr = coerce_text(proc.stderr)
    except subprocess.TimeoutExpired as exc:
        timeout = True
        exit_code = None
        stdout = coerce_text(exc.stdout)
        stderr = coerce_text(exc.stderr)
    elapsed_ms = int((time.perf_counter() - started) * 1000)
    stdout_path.write_text(stdout, encoding="utf-8", errors="replace")
    stderr_path.write_text(stderr, encoding="utf-8", errors="replace")
    time.sleep(1.0)
    ccs = ccs_stats(provider_id, created_after_s)
    audit_rows = read_audit_rows(audit_start, model)
    audit = audit_stats(audit_rows)
    return RunResult(
        case_id=case_id,
        model=model,
        provider_id=provider_id,
        cwd=str(cwd),
        prompt_sha256=sha256_text(prompt),
        exit_code=exit_code,
        elapsed_ms=elapsed_ms,
        stdout_bytes=len(stdout.encode("utf-8", errors="replace")),
        stderr_bytes=len(stderr.encode("utf-8", errors="replace")),
        stdout_sha256=sha256_text(stdout),
        stderr_sha256=sha256_text(stderr),
        stdout_path=str(stdout_path),
        stderr_path=str(stderr_path),
        timeout=timeout,
        ccs_rows=ccs["rows"],
        ccs_ok_rows=ccs["ok_rows"],
        ccs_error_rows=ccs["error_rows"],
        ccs_input_tokens=ccs["input_tokens"],
        ccs_cache_read_tokens=ccs["cache_read_tokens"],
        ccs_cache_creation_tokens=ccs["cache_creation_tokens"],
        ccs_latency_ms_p50=ccs["latency_ms_p50"],
        ccs_first_token_ms_p50=ccs["first_token_ms_p50"],
        audit_rows=audit["rows"],
        audit_ok_rows=audit["ok_rows"],
        audit_error_rows=audit["error_rows"],
        audit_read_tokens=audit["read_tokens"],
        audit_miss_tokens=audit["miss_tokens"],
        audit_r2_pct=audit["r2_pct"],
        audit_ttft_ms_p50=audit["ttft_ms_p50"],
        audit_ttft_ms_p90=audit["ttft_ms_p90"],
    )


def write_report(run_dir: Path, results: list[RunResult], backups: dict[str, str]) -> None:
    raw = [dataclasses.asdict(result) for result in results]
    (run_dir / "results.json").write_text(
        json.dumps({"backups": backups, "results": raw}, ensure_ascii=False, indent=2) + "\n",
        encoding="utf-8",
    )
    lines = [
        "# cc-switch project matrix",
        "",
        f"- run_dir: `{run_dir}`",
        f"- local_zen: `{ZEN_BASE_URL}`",
        f"- backups: `{backups['settings']}`, `{backups['db']}`",
        "",
        "| model | case | exit | elapsed_s | ccs_rows | ccs_cache_read | ccs_input | audit_rows | audit_r2 | audit_ttft_p50 | audit_ttft_p90 |",
        "|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|",
    ]
    for item in results:
        lines.append(
            "| "
            + " | ".join(
                [
                    item.model,
                    item.case_id,
                    "timeout" if item.timeout else str(item.exit_code),
                    f"{item.elapsed_ms / 1000:.1f}",
                    str(item.ccs_rows),
                    str(item.ccs_cache_read_tokens),
                    str(item.ccs_input_tokens),
                    str(item.audit_rows),
                    "" if item.audit_r2_pct is None else f"{item.audit_r2_pct:.2f}%",
                    "" if item.audit_ttft_ms_p50 is None else str(item.audit_ttft_ms_p50),
                    "" if item.audit_ttft_ms_p90 is None else str(item.audit_ttft_ms_p90),
                ]
            )
            + " |"
        )
    (run_dir / "summary.md").write_text("\n".join(lines) + "\n", encoding="utf-8")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--run-id", default=utc_run_id())
    parser.add_argument("--timeout-s", type=int, default=1800)
    parser.add_argument("--models", nargs="+", default=list(PROVIDERS))
    parser.add_argument("--cases", nargs="+", default=[case_id for case_id, _, _ in CASES])
    parser.add_argument("--smoke", action="store_true", help="run only first selected model/case")
    parser.add_argument("--restart-ccs", action="store_true", help="restart cc-switch after provider changes")
    parser.add_argument("--no-restore", action="store_true", help="leave selected provider active")
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    verify_paths()
    healthcheck_local_zen()
    selected_cases = [case for case in CASES if case[0] in set(args.cases)]
    selected_models = [model for model in args.models if model in PROVIDERS]
    if args.smoke:
        selected_models = selected_models[:1]
        selected_cases = selected_cases[:1]
    if not selected_models or not selected_cases:
        raise SystemExit("no selected models/cases")
    run_dir = LOCAL_DEV / "runs" / f"ccswitch-project-matrix-{args.run_id}"
    run_dir.mkdir(parents=True, exist_ok=True)
    backups = backup_ccswitch(run_dir)
    original_provider = get_current_provider()
    results: list[RunResult] = []
    try:
        for model in selected_models:
            provider_id = PROVIDERS[model]
            print(f"=== switch provider {provider_id} model={model} ===", flush=True)
            switch_provider(provider_id)
            if args.restart_ccs:
                print("=== restart cc-switch ===", flush=True)
                restart_ccswitch()
            time.sleep(2.0)
            for case_id, cwd, prompt in selected_cases:
                print(f"=== run model={model} case={case_id} cwd={cwd} ===", flush=True)
                result = run_case(run_dir, case_id, cwd, prompt, model, provider_id, args.timeout_s)
                results.append(result)
                write_report(run_dir, results, backups)
                print(
                    f"done model={model} case={case_id} exit={result.exit_code} "
                    f"elapsed={result.elapsed_ms/1000:.1f}s audit_r2={result.audit_r2_pct}",
                    flush=True,
                )
    finally:
        if not args.no_restore and original_provider:
            print(f"=== restore provider {original_provider} ===", flush=True)
            with contextlib.suppress(Exception):
                switch_provider(original_provider)
                if args.restart_ccs:
                    restart_ccswitch()
    write_report(run_dir, results, backups)
    print(f"summary: {run_dir / 'summary.md'}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
