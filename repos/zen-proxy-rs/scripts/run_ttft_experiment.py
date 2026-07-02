#!/usr/bin/env python3
"""
Run a repeatable streaming TTFT experiment with generated payloads.

The script is conservative by default: --tokens 1000. For 100K/200K tests set
--tokens explicitly and use a budgeted test window.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import statistics
import time
import urllib.error
import urllib.request
from pathlib import Path
from typing import Any


DEFAULT_TOKEN_BUDGET = 1000
DEFAULT_REQUEST_BUDGET = 1


def generated_text(approx_tokens: int) -> str:
    # Rough English-like token approximation: one short word per token.
    unit = "alpha beta gamma delta epsilon zeta eta theta "
    words_needed = max(1, approx_tokens)
    repeats = (words_needed // 8) + 1
    return " ".join((unit * repeats).split()[:words_needed])


def token_bucket_name(approx_tokens: int) -> str:
    if approx_tokens >= 1000 and approx_tokens % 1000 == 0:
        return f"{approx_tokens // 1000}k"
    return str(approx_tokens)


def normalized_error(message: str) -> str:
    if not message:
        return ""
    digest = hashlib.sha256(message.encode("utf-8", errors="replace")).hexdigest()[:12]
    first_line = message.splitlines()[0][:80]
    return f"{first_line} [sha256:{digest}]"


def build_payload(model: str, approx_tokens: int) -> dict[str, Any]:
    return {
        "model": model,
        "stream": True,
        "messages": [
            {"role": "system", "content": "Answer with a concise checksum-style summary."},
            {"role": "user", "content": generated_text(approx_tokens)},
        ],
        "max_tokens": 32,
    }


def run_once(
    base_url: str,
    key: str,
    model: str,
    approx_tokens: int,
    timeout: float,
    dry_run: bool,
) -> dict[str, Any]:
    payload = build_payload(model, approx_tokens)
    body = json.dumps(payload).encode("utf-8")
    if dry_run:
        return {
            "approx_tokens": approx_tokens,
            "body_bytes": len(body),
            "status": "dry-run",
            "first_byte": 0,
            "first_content": 0,
            "total": 0,
            "bytes_received": 0,
            "error": "",
        }

    headers = {
        "Authorization": f"Bearer {key}",
        "Content-Type": "application/json",
        "Accept": "text/event-stream",
    }
    req = urllib.request.Request(base_url.rstrip() + "/v1/chat/completions", data=body, headers=headers)
    started = time.perf_counter()
    first_byte_ms = 0
    first_content_ms = 0
    bytes_recv = 0
    status: int | str = 0
    error = ""
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            status = resp.getcode()
            while True:
                line = resp.readline()
                if not line:
                    break
                bytes_recv += len(line)
                if first_byte_ms == 0:
                    first_byte_ms = int((time.perf_counter() - started) * 1000)
                if first_content_ms == 0 and b"content" in line and b"delta" in line:
                    first_content_ms = int((time.perf_counter() - started) * 1000)
    except urllib.error.HTTPError as exc:
        status = exc.code
        error = exc.read().decode("utf-8", errors="replace")[:400]
    except Exception as exc:
        error = f"{type(exc).__name__}: {exc}"
    total_ms = int((time.perf_counter() - started) * 1000)
    return {
        "approx_tokens": approx_tokens,
        "body_bytes": len(body),
        "status": status,
        "first_byte": first_byte_ms,
        "first_content": first_content_ms,
        "total": total_ms,
        "bytes_received": bytes_recv,
        "error": normalized_error(error),
    }


def planned_cases(tokens: list[int], repeat: int, case_prefix: str, cold_warm: str) -> list[dict[str, Any]]:
    cases: list[dict[str, Any]] = []
    for token_index, token_count in enumerate(tokens, start=1):
        bucket = token_bucket_name(token_count)
        for attempt in range(1, repeat + 1):
            label = cold_warm
            if cold_warm == "auto":
                label = "cold" if attempt == 1 else "warm"
            cases.append(
                {
                    "case_id": f"{case_prefix}-{token_index:02d}-{bucket}-a{attempt:02d}",
                    "attempt": attempt,
                    "token_bucket": bucket,
                    "approx_tokens": token_count,
                    "cold_warm": label,
                }
            )
    return cases


def enforce_budget(cases: list[dict[str, Any]], max_total_tokens: int, max_requests: int) -> None:
    planned_tokens = sum(int(case["approx_tokens"]) for case in cases)
    planned_requests = len(cases)
    errors = []
    if planned_tokens > max_total_tokens:
        errors.append(f"planned approx tokens {planned_tokens} exceed --max-total-tokens {max_total_tokens}")
    if planned_requests > max_requests:
        errors.append(f"planned requests {planned_requests} exceed --max-requests {max_requests}")
    if errors:
        raise SystemExit("Budget guard refused run: " + "; ".join(errors))


def write_json(path: Path, value: dict[str, Any]) -> None:
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")


def write_summary(path: Path, manifest: dict[str, Any], results: list[dict[str, Any]]) -> None:
    completed = [row for row in results if row["status"] != "dry-run"]
    ok = [row for row in completed if row["status"] == 200 and not row["error"]]
    first_content_values = [row["first_content"] for row in ok if row["first_content"]]
    first_byte_values = [row["first_byte"] for row in ok if row["first_byte"]]

    def median(values: list[int]) -> str:
        return str(int(statistics.median(values))) if values else "n/a"

    lines = [
        f"# TTFT Experiment {manifest['run_id']}",
        "",
        f"- model: `{manifest['model']}`",
        f"- base_url: `{manifest['base_url']}`",
        f"- dry_run: `{manifest['dry_run']}`",
        f"- cases: {len(results)}",
        f"- planned_approx_tokens: {manifest['planned_approx_tokens']}",
        f"- success_200: {len(ok)}/{len(completed) if completed else len(results)}",
        f"- first_byte_median_ms: {median(first_byte_values)}",
        f"- first_content_median_ms: {median(first_content_values)}",
        "",
        "| case_id | attempt | token_bucket | cold_warm | status | body_bytes | first_byte | first_content | total | error |",
        "| --- | ---: | --- | --- | --- | ---: | ---: | ---: | ---: | --- |",
    ]
    for row in results:
        error = str(row["error"]).replace("|", "/")
        lines.append(
            f"| {row['case_id']} | {row['attempt']} | {row['token_bucket']} | {row['cold_warm']} | "
            f"{row['status']} | {row['body_bytes']} | {row['first_byte']} | "
            f"{row['first_content']} | {row['total']} | {error} |"
        )
    path.write_text("\n".join(lines) + "\n", encoding="utf-8")


def main() -> int:
    parser = argparse.ArgumentParser(description="Run generated long-context TTFT probes.")
    parser.add_argument("--run-id", default=time.strftime("%Y%m%d-%H%M%S") + "-ttft")
    parser.add_argument("--base-url", default=os.getenv("NEWAPI_BASE_URL", "http://127.0.0.1:8081"))
    parser.add_argument("--api-key", default=os.getenv("NEWAPI_API_KEY", "sk-dev"))
    parser.add_argument("--model", default=os.getenv("ZEN_TEST_MODEL", "deepseek-v4-flash"))
    parser.add_argument("--tokens", type=int, nargs="+", default=[1000])
    parser.add_argument("--repeat", type=int, default=1)
    parser.add_argument("--case-prefix", default="ttft")
    parser.add_argument("--cold-warm", choices=["auto", "cold", "warm"], default="auto")
    parser.add_argument("--max-total-tokens", type=int, default=DEFAULT_TOKEN_BUDGET)
    parser.add_argument("--max-requests", type=int, default=DEFAULT_REQUEST_BUDGET)
    parser.add_argument("--timeout", type=float, default=180.0)
    parser.add_argument("--out-dir", default="test-records/runs")
    parser.add_argument("--dry-run", action="store_true")
    args = parser.parse_args()

    if args.repeat < 1:
        raise SystemExit("--repeat must be >= 1")
    if any(tokens < 1 for tokens in args.tokens):
        raise SystemExit("--tokens values must be >= 1")

    cases = planned_cases(args.tokens, args.repeat, args.case_prefix, args.cold_warm)
    enforce_budget(cases, args.max_total_tokens, args.max_requests)

    run_dir = Path(args.out_dir) / args.run_id
    run_dir.mkdir(parents=True, exist_ok=True)
    metrics_path = run_dir / "ttft-metrics.jsonl"
    manifest = {
        "run_id": args.run_id,
        "model": args.model,
        "base_url": args.base_url,
        "created_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "dry_run": args.dry_run,
        "repeat": args.repeat,
        "case_prefix": args.case_prefix,
        "cold_warm": args.cold_warm,
        "tokens": args.tokens,
        "planned_requests": len(cases),
        "planned_approx_tokens": sum(int(case["approx_tokens"]) for case in cases),
        "max_total_tokens": args.max_total_tokens,
        "max_requests": args.max_requests,
        "outputs": ["manifest.json", "summary.md", "ttft-metrics.jsonl"],
    }
    write_json(run_dir / "manifest.json", manifest)

    results: list[dict[str, Any]] = []
    with metrics_path.open("w", encoding="utf-8") as fh:
        for case in cases:
            result = run_once(
                args.base_url,
                args.api_key,
                args.model,
                int(case["approx_tokens"]),
                args.timeout,
                args.dry_run,
            )
            result = {
                "run_id": args.run_id,
                "case_id": case["case_id"],
                "attempt": case["attempt"],
                "token_bucket": case["token_bucket"],
                "cold_warm": case["cold_warm"],
                "model": args.model,
                **result,
            }
            results.append(result)
            fh.write(json.dumps(result, ensure_ascii=False) + "\n")
            print(json.dumps(result, ensure_ascii=False))
    write_summary(run_dir / "summary.md", manifest, results)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
