#!/usr/bin/env python3
"""Run a fixed NewAPI cache canary for route and R2 validation.

This is intentionally smaller than the project matrix. It sends a stable
Anthropic Messages body with no tools, then compares the provider audit rows
for the requested model. The first exact audit row is treated as warmup by
default; the steady rows are the cache gate.
"""

from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import importlib.util
import json
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parents[2]
RUN_ROOT = ROOT / ".local-dev" / "runs"
MATRIX_PATH = Path(__file__).with_name("run_newapi_dualhost_project_matrix.py")

spec = importlib.util.spec_from_file_location("newapi_dualhost_matrix", MATRIX_PATH)
if spec is None or spec.loader is None:
    raise RuntimeError(f"failed to load {MATRIX_PATH}")
matrix = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = matrix
spec.loader.exec_module(matrix)


def sha256_text(value: str) -> str:
    return hashlib.sha256(value.encode("utf-8", errors="replace")).hexdigest()


def canary_prompt(target_chars: int) -> str:
    seed = (
        "ZENPROXY_CACHE_CANARY_V1\n"
        "Stable cache material. No tools. No project scan. "
        "Reply with exactly CACHE_CANARY_OK.\n"
    )
    line = "cache-window-stability-route-identity-provider-prefix-0000000000\n"
    repeat = max(1, (target_chars - len(seed)) // len(line) + 1)
    return (seed + line * repeat)[:target_chars]


def post_anthropic_message(
    *,
    base_url: str,
    api_key: str,
    model: str,
    prompt: str,
    timeout_s: int,
    max_tokens: int,
) -> dict[str, Any]:
    payload = {
        "model": model,
        "messages": [{"role": "user", "content": prompt}],
        "stream": False,
        "max_tokens": max_tokens,
    }
    body = json.dumps(payload, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
    request = urllib.request.Request(
        base_url.rstrip("/") + "/v1/messages?beta=true",
        data=body,
        headers={
            "Authorization": f"Bearer {api_key}",
            "x-api-key": api_key,
            "anthropic-version": "2023-06-01",
            "x-fmc-client": "claude-code",
            "content-type": "application/json",
        },
        method="POST",
    )
    started = time.perf_counter()
    status = 0
    response_text = ""
    error = None
    try:
        with urllib.request.urlopen(request, timeout=timeout_s) as response:
            status = int(response.status)
            response_text = response.read().decode("utf-8", errors="replace")
    except urllib.error.HTTPError as exc:
        status = int(exc.code)
        response_text = exc.read().decode("utf-8", errors="replace")
        error = "http_error"
    except Exception as exc:  # noqa: BLE001 - this is a diagnostic CLI.
        error = type(exc).__name__
        response_text = str(exc)
    elapsed_ms = int((time.perf_counter() - started) * 1000)
    parsed: dict[str, Any] | None = None
    try:
        parsed = json.loads(response_text)
    except json.JSONDecodeError:
        parsed = None
    usage = parsed.get("usage") if isinstance(parsed, dict) and isinstance(parsed.get("usage"), dict) else {}
    return {
        "status": status,
        "ok": 200 <= status < 300,
        "elapsed_ms": elapsed_ms,
        "error": error,
        "response_preview": response_text[:500],
        "response_sha256": sha256_text(response_text),
        "response_bytes": len(response_text.encode("utf-8", errors="replace")),
        "response_model": parsed.get("model") if isinstance(parsed, dict) else None,
        "usage": usage,
    }


def post_panda_zenproxy_message(
    *,
    model: str,
    prompt: str,
    timeout_s: int,
    max_tokens: int,
) -> dict[str, Any]:
    payload = json.dumps(
        {
            "model": model,
            "messages": [{"role": "user", "content": prompt}],
            "stream": False,
            "max_tokens": max_tokens,
        },
        ensure_ascii=False,
        separators=(",", ":"),
    )
    remote = (
        "set -euo pipefail; "
        "set -a; . /etc/zen-proxy-rs/common.env; set +a; "
        f"cat > /tmp/newapi-cache-canary-payload.json <<'JSON'\n{payload}\nJSON\n"
        f"code=$(curl -sS --max-time {int(timeout_s)} -o /tmp/newapi-cache-canary-response.json "
        "-w '%{http_code}' "
        "-H \"Authorization: Bearer ${PROXY_API_KEY}\" "
        "-H 'x-fmc-client: claude-code' "
        "-H 'Content-Type: application/json' "
        "--data-binary @/tmp/newapi-cache-canary-payload.json "
        "http://127.0.0.1:4000/v1/messages || true); "
        "printf '%s\\n' \"$code\"; "
        "head -c 4000 /tmp/newapi-cache-canary-response.json 2>/dev/null || true"
    )
    started = time.perf_counter()
    code, stdout, stderr = matrix.ssh_capture(remote, timeout_s=timeout_s + 20)
    elapsed_ms = int((time.perf_counter() - started) * 1000)
    lines = stdout.splitlines()
    status = 0
    if lines:
        try:
            status = int(lines[0].strip() or "0")
        except ValueError:
            status = 0
    response_text = "\n".join(lines[1:])
    parsed: dict[str, Any] | None = None
    try:
        parsed = json.loads(response_text)
    except json.JSONDecodeError:
        parsed = None
    usage = parsed.get("usage") if isinstance(parsed, dict) and isinstance(parsed.get("usage"), dict) else {}
    return {
        "status": status,
        "ok": code == 0 and 200 <= status < 300,
        "elapsed_ms": elapsed_ms,
        "error": None if code == 0 else (stderr or stdout).strip()[:500],
        "response_preview": response_text[:500],
        "response_sha256": sha256_text(response_text),
        "response_bytes": len(response_text.encode("utf-8", errors="replace")),
        "response_model": parsed.get("model") if isinstance(parsed, dict) else None,
        "usage": usage,
    }


def run_model(args: argparse.Namespace, model: str, run_dir: Path) -> dict[str, Any]:
    cfg = matrix.load_provider(model) if args.transport == "public-newapi" else None
    model_dir = run_dir / model
    model_dir.mkdir(parents=True, exist_ok=True)
    prompt = canary_prompt(args.prompt_chars)
    (model_dir / "prompt.sha256").write_text(sha256_text(prompt) + "\n", encoding="utf-8")

    start_offset, audit_offset_error = matrix.audit_offset()
    started = time.time()
    results = []
    for index in range(args.iterations):
        if args.transport == "public-newapi":
            assert cfg is not None
            result = post_anthropic_message(
                base_url=cfg.base_url,
                api_key=cfg.api_key,
                model=model,
                prompt=prompt,
                timeout_s=args.timeout_s,
                max_tokens=args.max_tokens,
            )
        else:
            result = post_panda_zenproxy_message(
                model=model,
                prompt=prompt,
                timeout_s=args.timeout_s,
                max_tokens=args.max_tokens,
            )
        result["iteration"] = index + 1
        results.append(result)
        print(
            json.dumps(
                {
                    "event": "canary_iteration",
                    "model": model,
                    "iteration": index + 1,
                    "status": result["status"],
                    "elapsed_s": round(result["elapsed_ms"] / 1000, 2),
                    "response_model": result["response_model"],
                },
                ensure_ascii=False,
            ),
            flush=True,
        )
        if index + 1 < args.iterations:
            time.sleep(args.sleep_s)

    time.sleep(args.audit_lag_s)
    if start_offset is None:
        audit_rows_window: list[dict[str, Any]] = []
        audit_read_error = "skipped: audit_offset_failed"
    else:
        audit_rows_window, audit_read_error = matrix.read_audit_since(start_offset)

    audit_rows_exact = [row for row in audit_rows_window if matrix.audit_model_matches(row, model)]
    audit_rows_mismatch = [row for row in audit_rows_window if not matrix.audit_model_matches(row, model)]
    steady_rows = audit_rows_exact[args.warmup_rows :]
    hard_failures: list[dict[str, Any]] = []
    if audit_offset_error:
        hard_failures.append({"code": "audit_offset_failed", "detail": audit_offset_error})
    if audit_read_error:
        hard_failures.append({"code": "audit_read_failed", "detail": audit_read_error})
    if not audit_rows_exact:
        hard_failures.append({"code": "audit_exact_model_empty", "detail": "no audit rows matched requested model identity"})
    if audit_rows_mismatch:
        hard_failures.append(
            {
                "code": "route_mismatch",
                "detail": "time-window audit rows include another model identity",
                "model_triplets": matrix.audit_model_triplets(audit_rows_mismatch),
            }
        )
    if any(not item["ok"] for item in results):
        hard_failures.append({"code": "http_request_failed", "detail": [item["status"] for item in results]})

    steady_summary = matrix.audit_summary(steady_rows)
    if steady_rows and steady_summary.get("r2_pct") is not None and steady_summary["r2_pct"] < args.min_steady_r2:
        hard_failures.append(
            {
                "code": "steady_r2_below_min",
                "detail": {"r2_pct": steady_summary["r2_pct"], "min_steady_r2": args.min_steady_r2},
            }
        )
    if not steady_rows:
        hard_failures.append({"code": "steady_audit_empty", "detail": {"warmup_rows": args.warmup_rows}})

    summary = {
        "model": model,
        "provider_id": cfg.provider_id if cfg is not None else "panda-zenproxy-local",
        "base_url": cfg.base_url if cfg is not None else "http://127.0.0.1:4000",
        "transport": args.transport,
        "started_at": started,
        "elapsed_s": round(time.time() - started, 1),
        "iterations": args.iterations,
        "warmup_rows": args.warmup_rows,
        "prompt_chars": args.prompt_chars,
        "results": results,
        "audit": matrix.audit_summary(audit_rows_exact),
        "steady_audit": steady_summary,
        "audit_time_window": matrix.audit_summary(audit_rows_window),
        "audit_route_mismatch_rows": len(audit_rows_mismatch),
        "audit_route_mismatch_triplets": matrix.audit_model_triplets(audit_rows_mismatch),
        "audit_offset_error": audit_offset_error,
        "audit_read_error": audit_read_error,
        "audit_route_verified": bool(audit_rows_exact) and not audit_rows_mismatch and not audit_read_error,
        "hard_failures": hard_failures,
    }
    (model_dir / "summary.json").write_text(json.dumps(summary, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    return summary


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--models", nargs="+", default=list(matrix.PROVIDERS))
    parser.add_argument("--iterations", type=int, default=3)
    parser.add_argument("--warmup-rows", type=int, default=1)
    parser.add_argument("--prompt-chars", type=int, default=40000)
    parser.add_argument("--max-tokens", type=int, default=32)
    parser.add_argument("--timeout-s", type=int, default=180)
    parser.add_argument("--transport", choices=["panda-zenproxy", "public-newapi"], default="panda-zenproxy")
    parser.add_argument("--sleep-s", type=float, default=2.0)
    parser.add_argument("--audit-lag-s", type=float, default=3.0)
    parser.add_argument("--min-steady-r2", type=float, default=85.0)
    parser.add_argument("--run-id", default=dt.datetime.now(dt.UTC).strftime("%Y%m%d-%H%M%S"))
    parser.add_argument("--skip-ready-gate", action="store_true")
    parser.add_argument("--ready-min-uptime-s", type=int, default=300)
    parser.add_argument("--ready-recent-window-s", type=int, default=300)
    parser.add_argument("--ready-max-proxy-errors", type=int, default=10)
    parser.add_argument("--allow-hard-failures", action="store_true")
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    if args.iterations <= args.warmup_rows:
        raise SystemExit("--iterations must be greater than --warmup-rows")
    run_dir = RUN_ROOT / f"newapi-cache-canary-{args.run_id}"
    run_dir.mkdir(parents=True, exist_ok=True)

    if not args.skip_ready_gate:
        try:
            ready_report = matrix.panda_ready_gate(
                min_uptime_s=args.ready_min_uptime_s,
                recent_window_s=args.ready_recent_window_s,
                max_proxy_errors=args.ready_max_proxy_errors,
            )
        except matrix.ReadyGateError as exc:
            (run_dir / "ready-gate.json").write_text(
                json.dumps(exc.report, ensure_ascii=False, indent=2) + "\n",
                encoding="utf-8",
            )
            print(json.dumps({"event": "ready_gate_failed", "report": exc.report}, ensure_ascii=False), flush=True)
            return 2
        (run_dir / "ready-gate.json").write_text(
            json.dumps(ready_report, ensure_ascii=False, indent=2) + "\n",
            encoding="utf-8",
        )
        print(json.dumps({"event": "ready_gate_passed", "report": ready_report}, ensure_ascii=False), flush=True)

    summaries = []
    hard_failure_count = 0
    for model in args.models:
        if model not in matrix.PROVIDERS:
            raise SystemExit(f"unsupported model: {model}")
        summary = run_model(args, model, run_dir)
        summaries.append(summary)
        hard_failure_count += len(summary["hard_failures"])
        print(
            json.dumps(
                {
                    "event": "canary_model_done",
                    "model": model,
                    "steady_audit": summary["steady_audit"],
                    "hard_failures": summary["hard_failures"],
                },
                ensure_ascii=False,
            ),
            flush=True,
        )

    (run_dir / "summary.json").write_text(json.dumps(summaries, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print(
        json.dumps(
            {"event": "done", "run_dir": str(run_dir), "hard_failure_count": hard_failure_count},
            ensure_ascii=False,
        ),
        flush=True,
    )
    if hard_failure_count and not args.allow_hard_failures:
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
