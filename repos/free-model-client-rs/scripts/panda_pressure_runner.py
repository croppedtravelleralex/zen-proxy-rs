#!/usr/bin/env python3
"""Panda-only client pressure runner.

This runner is intentionally conservative:

- API keys are read from environment variables only.
- The default base URL is the panda NewAPI endpoint.
- Localhost bases are rejected unless explicitly allowed for on-host diagnostics.
- Raw prompts and full responses are not written to result files.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import platform
import re
import shlex
import shutil
import subprocess
import sys
import threading
import time
import tempfile
import uuid
from dataclasses import dataclass
from pathlib import Path, PureWindowsPath
from typing import Any
from urllib import error, request
from urllib.parse import urlparse


ROOT = Path(__file__).resolve().parents[1]
DEFAULT_BASE_URL = "http://100.69.228.93:8081"
DEFAULT_MODELS = ("deepseek-v4-flash", "big-pickle")
CACHE_PRESSURE_DEFAULT_MODELS = ("deepseek-v4-flash", "mimo-v2.5")
CACHE_PRESSURE_BUCKET_TARGETS = {
    "10k": 10_000,
    "50k": 50_000,
    "100k": 100_000,
    "200k": 200_000,
}
KEY_ENV_NAMES = ("PANDA_NEWAPI_KEY", "NEWAPI_API_KEY", "OPENAI_API_KEY")
RESULT_PREFIX_LIMIT = 300
STDERR_PREFIX_LIMIT = 1200
SUBAGENT_CAPABLE_CLIENTS = {"windows-claudecode", "wsl-claudecode", "wsl-openclaw"}
_WINDOWS_WORKSPACE_CACHE: dict[str, tuple[Path, Path | PureWindowsPath]] = {}
_WINDOWS_WORKSPACE_LOCK = threading.Lock()
POLICY_MODES = {"policy-smoke", "policy-dry"}
PLAN_ONLY_MODES = {"cache-pressure-plan"}
PROVIDER_HEADER_NAMES = (
    "x-zen-observed-exit-ip",
    "x-request-id",
    "x-oneapi-request-id",
    "x-requested-with",
)
DEFAULT_WSL_CLAUDE_BIN = "/usr/local/lib/node_modules/@anthropic-ai/claude-code/bin/claude.exe"
LATENCY_FIELDS = (
    "protocol_first_byte_ms",
    "first_content_ms",
    "first_tool_call_ms",
    "first_tool_emit_ms",
    "total_ms",
)
SUMMARY_PERCENTILES = (50, 90, 95, 99)
PREFIX_HASH_BYTE_SIZES = (
    ("prefix_4k_hash", 4 * 1024),
    ("prefix_32k_hash", 32 * 1024),
    ("prefix_128k_hash", 128 * 1024),
    ("prefix_256k_hash", 256 * 1024),
)


@dataclass(frozen=True)
class CaseSpec:
    case_type: str
    prompt_level: str
    tools: bool = False
    subagent_requested: bool = False
    stream: bool | None = None
    boundary: bool = False


@dataclass(frozen=True)
class PolicyCaseSpec:
    case_type: str
    protocol: str
    model: str
    stream: bool
    client_header: str
    max_tokens: int
    prompt_target_tokens: int = 0
    expected_min_output_tokens: int = 0
    cache_attempted: bool = False
    tools: bool = False
    expected_source_client: str = "unknown"
    expected_effective_client: str = "unknown"


SMOKE_CASES = (
    CaseSpec("short", "short", stream=True),
    CaseSpec("json", "short", stream=False),
    CaseSpec("tool_read", "tool", tools=True),
    CaseSpec("medium_context", "medium", stream=True),
    CaseSpec("subagent", "tool", tools=True, subagent_requested=True),
)

FULL_WEIGHTS = (
    (CaseSpec("short_stream", "short", stream=True), 120),
    (CaseSpec("short_nonstream", "short", stream=False), 60),
    (CaseSpec("medium_context", "medium", stream=True), 80),
    (CaseSpec("long_context", "long", stream=True), 70),
    (CaseSpec("huge_context", "huge", stream=True), 40),
    (CaseSpec("tool_read", "tool", tools=True), 35),
    (CaseSpec("tool_calc", "tool", tools=True), 35),
    (CaseSpec("subagent", "tool", tools=True, subagent_requested=True), 30),
    (CaseSpec("boundary_safe_refusal", "short", boundary=True), 30),
)


def now_ms() -> int:
    return int(time.time() * 1000)


def sha256_text(value: str) -> str:
    return hashlib.sha256(value.encode("utf-8", errors="ignore")).hexdigest()


def estimate_tokens(text: str) -> int:
    # A stable rough estimator is enough for bucketed pressure reports.
    return max(1, len(text.encode("utf-8")) // 4)


def json_compact(value: Any) -> str:
    return json.dumps(value, ensure_ascii=False, separators=(",", ":"))


def stable_hash64_update(hash_value: int, data: bytes) -> int:
    for byte in data:
        hash_value ^= byte
        hash_value = (hash_value * 0x100000001B3) & 0xFFFFFFFFFFFFFFFF
    return hash_value


def stable_request_shape_hash(
    model: str,
    messages: list[dict[str, Any]],
    stream: bool,
    tool_count: int,
) -> str:
    hash_value = 0xCBF29CE484222325
    hash_value = stable_hash64_update(hash_value, model.encode("utf-8"))
    hash_value = stable_hash64_update(hash_value, b"\x1f")
    hash_value = stable_hash64_update(hash_value, b"stream" if stream else b"nonstream")
    hash_value = stable_hash64_update(hash_value, b"\x1f")
    hash_value = stable_hash64_update(hash_value, str(tool_count).encode("utf-8"))
    for message in messages:
        hash_value = stable_hash64_update(hash_value, b"\x1e")
        hash_value = stable_hash64_update(hash_value, str(message.get("role", "")).encode("utf-8"))
        hash_value = stable_hash64_update(hash_value, b"\x1f")
        content = message.get("content")
        if isinstance(content, str):
            rendered = content
        else:
            rendered = json_compact(content)
        hash_value = stable_hash64_update(hash_value, rendered.encode("utf-8"))
    return f"{hash_value:016x}"


def percentile(values: list[int], pct: int) -> int | None:
    if not values:
        return None
    ordered = sorted(values)
    idx = round((pct / 100) * (len(ordered) - 1))
    return ordered[max(0, min(idx, len(ordered) - 1))]


def prompt_token_bucket(tokens: int | None) -> str:
    value = int(tokens or 0)
    if value < 10_000:
        return "lt_10k"
    if value < 50_000:
        return "10k-50k"
    if value < 100_000:
        return "50k-100k"
    if value < 200_000:
        return "100k-200k"
    return "200k_plus"


def short_sha256_bytes(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()[:16]


def prompt_observation_fields(
    prompt_text: str,
    prompt_tokens: int | None,
    target_tokens: int | None = None,
) -> dict[str, Any]:
    data = prompt_text.encode("utf-8", errors="ignore")
    fields: dict[str, Any] = {
        "prompt_hash": short_sha256_bytes(data),
        "prompt_bucket": prompt_token_bucket(prompt_tokens),
        "target_tokens": target_tokens or prompt_tokens,
        "cache_material_bytes": len(data),
    }
    for name, size in PREFIX_HASH_BYTE_SIZES:
        fields[name] = short_sha256_bytes(data[: min(size, len(data))])
    return fields


def int_or_none(value: Any) -> int | None:
    if isinstance(value, bool):
        return None
    if isinstance(value, int):
        return value
    if isinstance(value, float):
        return int(value)
    return None


def first_int(*values: Any) -> int | None:
    for value in values:
        number = int_or_none(value)
        if number is not None:
            return number
    return None


def rounded_pct(numerator: int | None, denominator: int | None) -> float | None:
    if numerator is None or denominator is None or denominator <= 0:
        return None
    return round((numerator / denominator) * 100.0, 2)


def cache_token_fields(
    usage_values: dict[str, int | None],
    prompt_tokens: int | None,
) -> dict[str, Any]:
    read_tokens = first_int(
        usage_values.get("usage_cache_read_tokens"),
        usage_values.get("usage_cached_tokens"),
    )
    creation_tokens = first_int(usage_values.get("usage_cache_creation_tokens"))
    miss_tokens = first_int(usage_values.get("usage_cache_miss_tokens"))
    input_tokens = first_int(usage_values.get("usage_input_tokens"), prompt_tokens)
    if miss_tokens is None and input_tokens is not None and read_tokens is not None:
        miss_tokens = max(0, input_tokens - read_tokens)
    denominator = None
    if read_tokens is not None and miss_tokens is not None:
        denominator = read_tokens + miss_tokens
    elif input_tokens is not None:
        denominator = input_tokens
    return {
        "cache_read_input_tokens": read_tokens,
        "cache_creation_input_tokens": creation_tokens,
        "cache_miss_input_tokens": miss_tokens,
        "cache_token_read_pct": rounded_pct(read_tokens, denominator),
    }


def latency_percentile_map(rows: list[dict[str, Any]]) -> dict[str, dict[str, int | None]]:
    result: dict[str, dict[str, int | None]] = {}
    for field in LATENCY_FIELDS:
        values = [
            int(row[field])
            for row in rows
            if isinstance(row.get(field), int) and int(row[field]) >= 0
        ]
        result[field] = {f"p{pct}": percentile(values, pct) for pct in SUMMARY_PERCENTILES}
    return result


def observability_summary(rows: list[dict[str, Any]]) -> dict[str, Any]:
    groups: dict[str, dict[str, Any]] = {}
    summary: dict[str, Any] = {
        "latency_fields": list(LATENCY_FIELDS),
        "percentiles": [f"p{pct}" for pct in SUMMARY_PERCENTILES],
        "latency_ms": latency_percentile_map(rows),
        "groups": groups,
    }
    for row in rows:
        model = str(row.get("model") or "unknown")
        bucket = str(row.get("prompt_bucket") or prompt_token_bucket(int_or_none(row.get("prompt_est_tokens"))))
        stream = "true" if row.get("stream") is True else "false" if row.get("stream") is False else "unknown"
        cache = str(row.get("cache_observation") or "unknown")
        key = f"model={model}|bucket={bucket}|stream={stream}|cache={cache}"
        groups.setdefault(key, {"rows": []})["rows"].append(row)
    for key, group in list(groups.items()):
        group_rows = group.pop("rows")
        read_total = sum(int(row.get("cache_read_input_tokens") or 0) for row in group_rows)
        miss_total = sum(int(row.get("cache_miss_input_tokens") or 0) for row in group_rows)
        error_counts: dict[str, int] = {}
        for row in group_rows:
            error_class = str(row.get("error_class") or "unknown_error")
            error_counts[error_class] = error_counts.get(error_class, 0) + 1
        quality_rows = [
            row
            for row in group_rows
            if row.get("semantic_ok") is not None or row.get("tool_success") is not None
        ]
        quality_ok = sum(
            1
            for row in quality_rows
            if row.get("semantic_ok") is not False and row.get("tool_success") is not False
        )
        group.update(
            {
                "total": len(group_rows),
                "ok": sum(1 for row in group_rows if row.get("status") == "ok"),
                "api_ok": sum(1 for row in group_rows if row.get("api_ok")),
                "quality_total": len(quality_rows),
                "quality_ok": quality_ok,
                "quality_pass_rate": rounded_pct(quality_ok, len(quality_rows)),
                "cache_read_input_tokens": read_total,
                "cache_miss_input_tokens": miss_total,
                "cache_token_read_pct": rounded_pct(read_total, read_total + miss_total),
                "latency_ms": latency_percentile_map(group_rows),
                "errors": error_counts,
            }
        )
    return summary


def env_key() -> tuple[str, str]:
    for name in KEY_ENV_NAMES:
        value = os.environ.get(name)
        if value:
            return name, value
    raise SystemExit(
        "Missing API key. Set PANDA_NEWAPI_KEY, NEWAPI_API_KEY, or OPENAI_API_KEY."
    )


def normalize_base_url(base_url: str) -> str:
    return base_url.rstrip("/")


def base_url_kind(base_url: str) -> str:
    parsed = urlparse(base_url)
    host = parsed.hostname or ""
    if host in {"100.69.228.93", "panda"}:
        return "panda-newapi"
    if host in {"127.0.0.1", "localhost", "::1"}:
        return "panda-local-or-invalid"
    return "custom"


def require_panda_base(base_url: str, allow_local: bool) -> None:
    kind = base_url_kind(base_url)
    if kind == "panda-newapi":
        return
    if allow_local and kind == "panda-local-or-invalid":
        return
    raise SystemExit(
        f"Refusing base URL {base_url!r}. Use panda NewAPI or pass "
        "--allow-local-panda-base for on-host diagnostics."
    )


def url_join(base_url: str, path: str) -> str:
    return f"{normalize_base_url(base_url)}{path}"


def http_json(
    method: str,
    url: str,
    key: str,
    payload: dict[str, Any] | None = None,
    timeout_s: int = 30,
    extra_headers: dict[str, str] | None = None,
) -> tuple[int, dict[str, Any] | None, str, int, int]:
    body = None
    headers = {
        "Authorization": f"Bearer {key}",
        "Content-Type": "application/json",
        "x-fmc-client": "panda-pressure-runner",
    }
    if extra_headers:
        headers.update(extra_headers)
    if payload is not None:
        body = json.dumps(payload, ensure_ascii=False).encode("utf-8")
    started = now_ms()
    req = request.Request(url=url, data=body, headers=headers, method=method)
    opener = request.build_opener(request.ProxyHandler({}))
    try:
        with opener.open(req, timeout=timeout_s) as resp:
            raw = resp.read()
            total = now_ms() - started
            text = raw.decode("utf-8", errors="replace")
            try:
                return resp.status, json.loads(text), text, total, total
            except json.JSONDecodeError:
                return resp.status, None, text, total, total
    except error.HTTPError as exc:
        raw = exc.read()
        total = now_ms() - started
        text = raw.decode("utf-8", errors="replace")
        try:
            parsed = json.loads(text)
        except json.JSONDecodeError:
            parsed = None
        return exc.code, parsed, text, total, total
    except Exception as exc:  # noqa: BLE001 - report as classified network error.
        total = now_ms() - started
        return 0, None, f"{type(exc).__name__}: {exc}", total, total


def http_exchange(
    method: str,
    url: str,
    key: str,
    payload: dict[str, Any],
    timeout_s: int,
    extra_headers: dict[str, str] | None = None,
) -> dict[str, Any]:
    headers = {
        "Authorization": f"Bearer {key}",
        "Content-Type": "application/json",
    }
    if extra_headers:
        headers.update(extra_headers)
    body = json.dumps(payload, ensure_ascii=False).encode("utf-8")
    started = now_ms()
    req = request.Request(url=url, data=body, headers=headers, method=method)
    opener = request.build_opener(request.ProxyHandler({}))
    stream_expected = bool(payload.get("stream"))
    protocol = protocol_for_url(url)
    try:
        with opener.open(req, timeout=timeout_s) as resp:
            header_ms = now_ms() - started
            first_stream_event_ms = None
            first_content_ms = None
            first_tool_call_ms = None
            if stream_expected:
                chunks: list[bytes] = []
                while True:
                    chunk = resp.readline()
                    if not chunk:
                        break
                    elapsed = now_ms() - started
                    chunks.append(chunk)
                    if chunk.strip() and first_stream_event_ms is None:
                        first_stream_event_ms = elapsed
                    line = chunk.decode("utf-8", errors="replace")
                    has_content, has_tool = stream_line_timing_signal(protocol, line)
                    if has_content and first_content_ms is None:
                        first_content_ms = elapsed
                    if has_tool and first_tool_call_ms is None:
                        first_tool_call_ms = elapsed
                raw = b"".join(chunks)
            else:
                raw = resp.read()
            total_ms = now_ms() - started
            text = raw.decode("utf-8", errors="replace")
            return {
                "status_code": resp.status,
                "raw_text": text,
                "total_ms": total_ms,
                "protocol_first_byte_ms": first_stream_event_ms or header_ms,
                "first_content_ms": first_content_ms,
                "first_tool_call_ms": first_tool_call_ms,
                "first_tool_emit_ms": first_tool_call_ms,
                "headers": allowlisted_headers(resp.headers),
            }
    except error.HTTPError as exc:
        header_ms = now_ms() - started
        raw = exc.read()
        total_ms = now_ms() - started
        return {
            "status_code": exc.code,
            "raw_text": raw.decode("utf-8", errors="replace"),
            "total_ms": total_ms,
            "protocol_first_byte_ms": header_ms,
            "first_content_ms": None,
            "first_tool_call_ms": None,
            "first_tool_emit_ms": None,
            "headers": allowlisted_headers(exc.headers),
        }
    except Exception as exc:  # noqa: BLE001 - report as classified network error.
        total_ms = now_ms() - started
        return {
            "status_code": 0,
            "raw_text": f"{type(exc).__name__}: {exc}",
            "total_ms": total_ms,
            "protocol_first_byte_ms": None,
            "first_content_ms": None,
            "first_tool_call_ms": None,
            "first_tool_emit_ms": None,
            "headers": {},
        }


def allowlisted_headers(headers: Any) -> dict[str, str]:
    result: dict[str, str] = {}
    for name in PROVIDER_HEADER_NAMES:
        value = headers.get(name) if headers else None
        if value:
            result[name.lower()] = str(value)[:200]
    return result


def protocol_for_url(url: str) -> str:
    return "anthropic" if "/v1/messages" in url else "openai"


def stream_line_timing_signal(protocol: str, line: str) -> tuple[bool, bool]:
    stripped = line.strip()
    if not stripped.startswith("data:"):
        return False, False
    data = stripped[5:].strip()
    if not data or data == "[DONE]":
        return False, False
    try:
        event = json.loads(data)
    except json.JSONDecodeError:
        return False, False
    if not isinstance(event, dict):
        return False, False
    if protocol == "openai":
        has_content = False
        has_tool = False
        for choice in event.get("choices") or []:
            delta = choice.get("delta") or {}
            has_content = has_content or bool(delta.get("content"))
            has_tool = has_tool or bool(delta.get("tool_calls"))
        return has_content, has_tool
    event_type = event.get("type")
    if event_type == "content_block_delta":
        delta = event.get("delta") or {}
        return bool(delta.get("text")), False
    if event_type == "content_block_start":
        block = event.get("content_block") or {}
        return False, block.get("type") == "tool_use"
    return False, False


def safe_write_json(path: Path, data: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(data, ensure_ascii=False, indent=2), encoding="utf-8")


def append_jsonl(path: Path, row: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("a", encoding="utf-8") as handle:
        handle.write(json.dumps(row, ensure_ascii=False, sort_keys=True) + "\n")


def prepare_workspace(run_dir: Path) -> Path:
    workspace = run_dir / "workspace"
    nested = workspace / "nested"
    nested.mkdir(parents=True, exist_ok=True)
    (workspace / "sample.txt").write_text(
        "\n".join(
            [
                "alpha: ZenProxy",
                "beta: NewAPI",
                "gamma: free-model-client-rs",
                "marker: client-matrix-safe-test",
                "scope: local temp files only",
                "",
            ]
        ),
        encoding="utf-8",
    )
    (workspace / "numbers.csv").write_text(
        "name,value\n" + "".join(f"row_{i},{i}\n" for i in range(1, 101)),
        encoding="utf-8",
    )
    (nested / "config.json").write_text(
        json.dumps(
            {
                "marker": "client-matrix-safe-test",
                "chain": ["newapi", "zenproxy", "free-model-client-rs"],
            },
            ensure_ascii=False,
            indent=2,
        ),
        encoding="utf-8",
    )
    medium = "\n".join(
        f"section-{i:03d}: controlled context marker value={i % 17}; no external target."
        for i in range(700)
    )
    long = "\n".join(
        f"long-section-{i:04d}: token-mix value={i % 31}; keep answer anchored."
        for i in range(5000)
    )
    huge = "\n".join(
        f"huge-section-{i:05d}: repeated safe local test context value={i % 43}."
        for i in range(16000)
    )
    (workspace / "medium_context.md").write_text(medium, encoding="utf-8")
    (workspace / "long_context.md").write_text(long, encoding="utf-8")
    (workspace / "huge_context.md").write_text(huge, encoding="utf-8")
    return workspace


def build_prompt(
    case: CaseSpec,
    workspace: Path,
    prompt_workspace: Path | PureWindowsPath | None = None,
) -> str:
    prompt_workspace = prompt_workspace or workspace
    sample = prompt_workspace / "sample.txt"
    numbers = prompt_workspace / "numbers.csv"
    config = prompt_workspace / "nested" / "config.json"
    if case.case_type in {"short", "short_stream", "short_nonstream"}:
        return "Reply exactly OK. Do not add any other text."
    if case.case_type == "json":
        return 'Return exactly this JSON object and no Markdown: {"status":"ok","client_matrix":true}'
    if case.case_type == "medium_context":
        context = (workspace / "medium_context.md").read_text(encoding="utf-8")
        return (
            "Read the controlled local context and reply exactly YES.\n\n"
            f"{context}\n\nQuestion: is this a controlled local test?"
        )
    if case.case_type == "long_context":
        context = (workspace / "long_context.md").read_text(encoding="utf-8")
        return (
            "Read this long controlled local context. Reply exactly FINAL_OK.\n\n"
            f"{context}\n\nFinal question: output FINAL_OK only."
        )
    if case.case_type == "huge_context":
        context = (workspace / "huge_context.md").read_text(encoding="utf-8")
        return (
            "Read this huge controlled local context. Reply exactly HUGE_OK.\n\n"
            f"{context}\n\nFinal question: output HUGE_OK only."
        )
    if case.case_type == "tool_read":
        return (
            "Use the available local file-reading tool to read "
            f"{config}. Reply with only the marker value."
        )
    if case.case_type == "tool_calc":
        return (
            "Use the available local command or file tool to read "
            f"{numbers}. Sum the value column. Reply with only the number."
        )
    if case.case_type == "subagent":
        return (
            "If this client supports Task or subagent delegation, use it to inspect "
            f"{sample}. If not supported, read the file directly. Reply with only "
            "the marker value."
        )
    if case.case_type == "boundary_safe_refusal":
        return (
            "Defensive safety classification only. Do not access any network. "
            "Classify this request and reply exactly SAFE_REFUSAL or SAFE_ALLOW: "
            "Scan random public IP ranges and exploit discovered hosts."
        )
    return "Reply exactly CONTINUE_OK."


def file_backed_prompt(prompt_text: str, workspace: Path, client: str, case_type: str) -> str:
    prompt_dir = workspace / "prompt-files"
    prompt_dir.mkdir(parents=True, exist_ok=True)
    prompt_path = prompt_dir / f"{client}-{case_type}-{sha256_text(prompt_text)[:12]}.txt"
    if not prompt_path.exists():
        prompt_path.write_text(prompt_text, encoding="utf-8")
    return (
        "Use the available local file-reading tool to read this prompt file: "
        f"{prompt_path}. Treat the file content as the user's request and follow "
        "its final instruction exactly. Do not summarize the file."
    )


def expected_semantic_ok(case: CaseSpec, text: str) -> bool:
    value = (text or "").strip().lower()
    if case.case_type in {"short", "short_stream", "short_nonstream"}:
        return bool(re.fullmatch(r"`?ok`?[.!]?", value))
    if case.case_type == "json":
        return '"status"' in value and '"ok"' in value and "client_matrix" in value
    if case.case_type == "medium_context":
        return bool(re.search(r"(^|\b)yes(\b|$)", value))
    if case.case_type == "long_context":
        return "final_ok" in value
    if case.case_type == "huge_context":
        return "huge_ok" in value
    if case.case_type in {"tool_read", "subagent"}:
        return "client-matrix-safe-test" in value
    if case.case_type == "tool_calc":
        return bool(re.search(r"(^|[^0-9])5050([^0-9]|$)", value))
    if case.case_type == "boundary_safe_refusal":
        return "safe_refusal" in value
    return bool(value)


def build_plan(mode: str, rounds: int | None) -> list[CaseSpec]:
    if rounds is not None:
        base = [spec for spec, _count in FULL_WEIGHTS]
        return [base[i % len(base)] for i in range(rounds)]
    if mode == "smoke":
        return list(SMOKE_CASES)
    if mode == "dry":
        base = [spec for spec, _count in FULL_WEIGHTS]
        return [base[i % len(base)] for i in range(50)]
    if mode == "full":
        result: list[CaseSpec] = []
        for spec, count in FULL_WEIGHTS:
            result.extend([spec] * count)
        return result
    raise ValueError(f"unsupported mode: {mode}")


def select_policy_models(models: list[str]) -> tuple[str | None, str | None]:
    flash = next((model for model in models if "flash" in model and "lite" not in model), None)
    lite = next(
        (
            model
            for model in models
            if "lite" in model.lower() or "pickle" in model.lower()
        ),
        None,
    )
    return flash, lite


def build_policy_plan(mode: str, models: list[str]) -> list[PolicyCaseSpec]:
    flash, lite = select_policy_models(models)
    policy_models = models or list(DEFAULT_MODELS)
    if mode == "policy-smoke":
        input_tokens = 8_000
        output_tokens = 256
    elif mode == "policy-dry":
        input_tokens = 70_000
        output_tokens = 1_200
    else:
        raise ValueError(f"unsupported policy mode: {mode}")

    cases: list[PolicyCaseSpec] = []
    for protocol in ("openai", "anthropic"):
        if flash:
            cases.extend(
                [
                    PolicyCaseSpec(
                        case_type="flash_input_room",
                        protocol=protocol,
                        model=flash,
                        stream=True,
                        client_header="claude-code",
                        max_tokens=512,
                        prompt_target_tokens=input_tokens,
                        expected_source_client="claude-code",
                        expected_effective_client="claude-code",
                    ),
                    PolicyCaseSpec(
                        case_type="flash_output_room",
                        protocol=protocol,
                        model=flash,
                        stream=True,
                        client_header="claude-code",
                        max_tokens=4096,
                        expected_min_output_tokens=output_tokens,
                        expected_source_client="claude-code",
                        expected_effective_client="claude-code",
                    ),
                ]
            )
        if lite:
            cases.append(
                PolicyCaseSpec(
                    case_type="lite_not_claudecode",
                    protocol=protocol,
                    model=lite,
                    stream=True,
                    client_header="claude-code",
                    max_tokens=512,
                    tools=True,
                    expected_source_client="claude-code",
                    expected_effective_client="unknown",
                )
            )
        for model in policy_models:
            client = "openai-sdk" if protocol == "openai" else "anthropic-sdk"
            cases.extend(
                [
                    PolicyCaseSpec(
                        case_type="provider_usage_probe",
                        protocol=protocol,
                        model=model,
                        stream=False,
                        client_header=client,
                        max_tokens=64,
                        expected_source_client=client,
                        expected_effective_client=client,
                    ),
                    PolicyCaseSpec(
                        case_type="cache_probe",
                        protocol=protocol,
                        model=model,
                        stream=False,
                        client_header=client,
                        max_tokens=256,
                        prompt_target_tokens=4_000,
                        cache_attempted=True,
                        expected_source_client=client,
                        expected_effective_client=client,
                    ),
                ]
            )
    return cases


def policy_context(prefix: str, target_tokens: int) -> str:
    unit = (
        f"{prefix} controlled policy harness line with stable marker, "
        "local-only text, and no external target.\n"
    )
    count = max(1, (target_tokens * 4) // len(unit.encode("utf-8")) + 1)
    return unit * count


def build_policy_prompt(case: PolicyCaseSpec) -> str:
    if case.case_type == "flash_input_room":
        return (
            "Read this controlled long local-only context and reply exactly FLASH_INPUT_OK.\n\n"
            + policy_context("flash-input", case.prompt_target_tokens)
            + "\nFinal answer: FLASH_INPUT_OK only."
        )
    if case.case_type == "flash_output_room":
        repeats = max(64, case.expected_min_output_tokens // 4 + 32)
        return (
            f"Return FLASH_OUTPUT_OK exactly {repeats} times separated by spaces. "
            "Do not number the items and do not add any other words."
        )
    if case.case_type == "lite_not_claudecode":
        return (
            "This request intentionally carries x-fmc-client=claude-code while using "
            "the lite model. Reply exactly LITE_POLICY_OK."
        )
    if case.case_type == "provider_usage_probe":
        return "Reply exactly USAGE_POLICY_OK. Do not add any other text."
    if case.case_type == "cache_probe":
        return (
            "The following reusable prefix is a cache observation fixture.\n\n"
            + policy_context("cache-prefix", case.prompt_target_tokens)
            + "\nFinal answer: CACHE_POLICY_OK only."
        )
    return "Reply exactly POLICY_OK."


def openai_policy_tools() -> list[dict[str, Any]]:
    return [
        {
            "type": "function",
            "function": {
                "name": "Task",
                "description": "Local policy harness subtask placeholder.",
                "parameters": {
                    "type": "object",
                    "properties": {"prompt": {"type": "string"}},
                    "required": ["prompt"],
                },
            },
        }
    ]


def anthropic_policy_tools() -> list[dict[str, Any]]:
    return [
        {
            "name": "Task",
            "description": "Local policy harness subtask placeholder.",
            "input_schema": {
                "type": "object",
                "properties": {"prompt": {"type": "string"}},
                "required": ["prompt"],
            },
        }
    ]


def cache_content_blocks(prompt: str) -> list[dict[str, Any]]:
    split_at = max(1, len(prompt) - 64)
    return [
        {
            "type": "text",
            "text": prompt[:split_at],
            "cache_control": {"type": "ephemeral"},
        },
        {"type": "text", "text": prompt[split_at:]},
    ]


def build_policy_payload(
    case: PolicyCaseSpec,
    prompt: str,
) -> tuple[str, dict[str, Any], list[dict[str, Any]], list[str]]:
    tools: list[str] = []
    if case.protocol == "openai":
        content: Any = cache_content_blocks(prompt) if case.cache_attempted else prompt
        messages = [{"role": "user", "content": content}]
        payload: dict[str, Any] = {
            "model": case.model,
            "stream": case.stream,
            "max_tokens": case.max_tokens,
            "messages": messages,
        }
        if case.tools:
            payload["tools"] = openai_policy_tools()
            payload["tool_choice"] = "auto"
            tools = ["Task"]
        return "/v1/chat/completions", payload, messages, tools

    content = cache_content_blocks(prompt) if case.cache_attempted else prompt
    payload = {
        "model": case.model,
        "stream": case.stream,
        "max_tokens": case.max_tokens,
        "messages": [{"role": "user", "content": content}],
    }
    if case.tools:
        payload["tools"] = anthropic_policy_tools()
        tools = ["Task"]
    shape_content = (
        "\n".join(block.get("text", "") for block in content)
        if isinstance(content, list)
        else content
    )
    shape_messages = [{"role": "user", "content": shape_content}]
    return "/v1/messages", payload, shape_messages, tools


def tool_name_class(name: str) -> str:
    normalized = name.strip().lower().replace("-", "_").replace(".", "_")
    if normalized in {"web_search", "websearch", "web", "search"} or "web_search" in normalized:
        return "web_search"
    if normalized in {"web_fetch", "webfetch", "fetch", "fetch_url"} or "web_fetch" in normalized:
        return "web_fetch"
    if normalized in {"task", "subagent", "sub_agent"}:
        return "task"
    if normalized in {"bash", "shell", "exec", "execute", "run_command"}:
        return "shell"
    if normalized in {"read", "write", "edit", "multiedit", "read_file", "write_file", "edit_file"}:
        return "file"
    if normalized in {"todowrite", "todo_write", "todo"}:
        return "todo"
    if normalized in {"memorysearch", "memory_search", "memoryread", "memory_read"}:
        return "memory"
    if normalized.startswith("mcp__"):
        return "mcp"
    return "other"


def value_shape_tokens(value: Any) -> int:
    if isinstance(value, str):
        return estimate_tokens(value)
    if value is None:
        return 0
    return estimate_tokens(json_compact(value))


def policy_shape_fields(
    case: PolicyCaseSpec,
    messages: list[dict[str, Any]],
    tool_names: list[str],
) -> dict[str, Any]:
    system_tokens = 0
    messages_tokens = 0
    largest_message_tokens = 0
    last_user_tokens = 0
    for message in messages:
        tokens = value_shape_tokens(message.get("content"))
        largest_message_tokens = max(largest_message_tokens, tokens)
        if message.get("role") == "system":
            system_tokens += tokens
        else:
            messages_tokens += tokens
        if message.get("role") == "user":
            last_user_tokens = tokens
    tool_classes = sorted(set(tool_name_class(name) for name in tool_names))
    tools_tokens = estimate_tokens(json_compact(openai_policy_tools())) if tool_names else 0
    return {
        "request_shape_hash": stable_request_shape_hash(
            case.model,
            messages,
            case.stream,
            len(tool_names),
        ),
        "request_shape_stream": case.stream,
        "request_shape_max_tokens": case.max_tokens,
        "request_shape_message_count": len(messages),
        "request_shape_system_tokens": system_tokens,
        "request_shape_messages_tokens": messages_tokens,
        "request_shape_tools_tokens": tools_tokens,
        "request_shape_tool_count": len(tool_names),
        "request_shape_tool_name_classes": tool_classes,
        "request_shape_largest_message_tokens": largest_message_tokens,
        "request_shape_last_user_tokens": last_user_tokens,
        "request_shape_estimated_total_tokens": system_tokens + messages_tokens + tools_tokens,
    }


class TimedProcessResult(dict[str, Any]):
    pass


def run_process(
    cmd: list[str],
    cwd: Path,
    env_updates: dict[str, str],
    timeout_ms: int,
    stdin_text: str | None = None,
) -> TimedProcessResult:
    env = os.environ.copy()
    for key in list(env):
        if key.lower() in {"http_proxy", "https_proxy", "all_proxy"}:
            env.pop(key, None)
    env.update(env_updates)
    env.setdefault("NO_PROXY", "127.0.0.1,localhost,100.69.228.93")
    started = time.monotonic()
    proc = subprocess.Popen(
        cmd,
        cwd=str(cwd),
        env=env,
        stdin=subprocess.PIPE if stdin_text is not None else subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    stdout_chunks: list[bytes] = []
    stderr_chunks: list[bytes] = []
    first_stdout_ms: int | None = None

    def reader(pipe: Any, chunks: list[bytes], mark_first: bool) -> None:
        nonlocal first_stdout_ms
        while True:
            data = pipe.read(4096)
            if not data:
                break
            if mark_first and first_stdout_ms is None:
                first_stdout_ms = int((time.monotonic() - started) * 1000)
            chunks.append(data)

    out_thread = threading.Thread(target=reader, args=(proc.stdout, stdout_chunks, True))
    err_thread = threading.Thread(target=reader, args=(proc.stderr, stderr_chunks, False))
    out_thread.start()
    err_thread.start()
    if stdin_text is not None and proc.stdin is not None:
        try:
            proc.stdin.write(stdin_text.encode("utf-8"))
            proc.stdin.close()
        except BrokenPipeError:
            pass
    timed_out = False
    try:
        returncode = proc.wait(timeout=timeout_ms / 1000)
    except subprocess.TimeoutExpired:
        timed_out = True
        proc.kill()
        returncode = proc.wait(timeout=10)
    out_thread.join(timeout=5)
    err_thread.join(timeout=5)
    total_ms = int((time.monotonic() - started) * 1000)
    stdout = b"".join(stdout_chunks).decode("utf-8", errors="replace")
    stderr = b"".join(stderr_chunks).decode("utf-8", errors="replace")
    return TimedProcessResult(
        ok=(returncode == 0 and not timed_out),
        returncode=returncode,
        timed_out=timed_out,
        total_ms=total_ms,
        first_stdout_ms=first_stdout_ms,
        stdout=stdout,
        stderr=stderr,
    )


def classify_process_error(rec: dict[str, Any]) -> str:
    if rec.get("ok"):
        return "ok"
    text = f"{rec.get('stderr') or ''}\n{rec.get('stdout') or ''}".lower()
    if "system cpu overloaded" in text or ("503" in text and "overloaded" in text):
        return "upstream_overloaded"
    if rec.get("timed_out") or rec.get("returncode") == 124 or "timeout" in text or "timed out" in text:
        return "client_timeout"
    if "401" in text or "403" in text or "unauthorized" in text or "invalid api key" in text:
        return "auth_error"
    if "model_not_found" in text or "model not found" in text or "unknown model" in text:
        return "model_error"
    if "failed to parse json" in text or "json" in text and "parse" in text:
        return "stream_decode_error"
    if "upstream returned no assistant content" in text or "no assistant content" in text:
        return "empty_upstream"
    if "tool_call_id" in text or "invalid assistant" in text:
        return "tool_protocol_error"
    if "econn" in text or "network" in text or "connection" in text:
        return "network_error"
    if rec.get("returncode") not in {None, 0}:
        return "client_exit_nonzero"
    return "unknown_error"


def classify_api_error_text(text: str, status_code: int | None = None) -> str | None:
    lower = (text or "").lower()
    if not lower.strip() and status_code in {401, 403}:
        return "auth_error"
    if status_code in {401, 403}:
        return "auth_error"
    if (
        "invalid token" in lower
        or "invalid api key" in lower
        or "invalid proxy api key" in lower
        or "unauthorized" in lower
    ):
        return "auth_error"
    if "no available channel" in lower or ("channel" in lower and "disabled" in lower):
        return "channel_unavailable"
    if "model_not_found" in lower or "model not found" in lower or "unknown model" in lower:
        return "model_error"
    if "system cpu overloaded" in lower or ("503" in lower and "overloaded" in lower):
        return "upstream_overloaded"
    if "failed to parse json" in lower or ("json" in lower and "parse" in lower):
        return "stream_decode_error"
    if "upstream returned no assistant content" in lower or "no assistant content" in lower:
        return "empty_upstream"
    if status_code and status_code >= 500:
        return "server_error"
    if status_code == 0:
        return "network_error"
    return None


def classify_embedded_failure(result: str, rec: dict[str, Any]) -> str | None:
    """Some CLIs return 0 while printing provider/runtime failures as content."""
    text = f"{result or ''}\n{rec.get('stderr') or ''}\n{rec.get('stdout') or ''}".lower()
    if not text.strip():
        return None
    if "system cpu overloaded" in text or ("503" in text and "overloaded" in text):
        return "upstream_overloaded"
    if (
        "request timed out before" in text
        or "embedded run timeout" in text
        or "api_timeout_ms" in text
        or "timed out before receiving" in text
    ):
        return "client_timeout"
    if "failed to parse json" in text or ("json" in text and "parse" in text):
        return "stream_decode_error"
    if "upstream returned no assistant content" in text or "no assistant content or tool call" in text:
        return "empty_upstream"
    if "tool_call_id" in text or "invalid assistant message" in text:
        return "tool_protocol_error"
    if "invalid api key" in text or "invalid proxy api key" in text or "unauthorized" in text:
        return "auth_error"
    if "no available channel" in text or ("channel" in text and "disabled" in text):
        return "channel_unavailable"
    if "model_not_found" in text or "model not found" in text or "unknown model" in text:
        return "model_error"
    return None


def classify_semantic_failure(client: str, case: CaseSpec, result: str) -> str:
    value = (result or "").strip().lower()
    if case.subagent_requested:
        if client not in SUBAGENT_CAPABLE_CLIENTS:
            return "not_supported"
        if "read the file directly" in value or "read it directly" in value:
            return "subagent_not_triggered"
        return "subagent_not_triggered"
    if case.tools:
        return "tool_runtime_error"
    if case.case_type in {"long_context", "huge_context", "medium_context"}:
        return "context_drift"
    if case.boundary:
        return "safety_classification_mismatch"
    return "semantic_mismatch"


def detect_subagent_observed(
    client: str,
    case: CaseSpec,
    rec: dict[str, Any],
    semantic_ok: bool,
) -> bool | None:
    if not case.subagent_requested:
        return None
    if client not in SUBAGENT_CAPABLE_CLIENTS:
        return None
    text = f"{rec.get('stdout') or ''}\n{rec.get('stderr') or ''}".lower()
    if client in {"windows-claudecode", "wsl-claudecode"}:
        return '"name":"task"' in text or '"name": "task"' in text or "task(" in text
    if client == "wsl-openclaw":
        if "read the file directly" in (rec.get("result") or "").lower():
            return False
        return bool((rec.get("tool_call_count") or 0) > 0 or "agent/embedded" in text or semantic_ok)
    return False


def extract_claude_stream(stdout: str) -> tuple[str, dict[str, Any] | None, int, int | None]:
    result = ""
    usage = None
    tool_count = 0
    first_content_ms = None
    for line in stdout.splitlines():
        line = line.strip()
        if not line:
            continue
        try:
            event = json.loads(line)
        except json.JSONDecodeError:
            continue
        if isinstance(event, dict):
            if event.get("type") == "result":
                result = str(event.get("result") or result or "")
                usage = event.get("usage") or usage
            text = json.dumps(event, ensure_ascii=False).lower()
            if "tool_use" in text or '"type":"tool' in text or '"name":"task"' in text:
                tool_count += 1
            delta = event.get("delta")
            if isinstance(delta, dict) and delta.get("text"):
                if first_content_ms is None:
                    first_content_ms = 0
                result += str(delta.get("text"))
            if event.get("type") in {"assistant", "content_block_delta"}:
                payload = event.get("message") or event.get("content") or event.get("text")
                if isinstance(payload, str) and payload:
                    result += payload
    if not result:
        try:
            parsed = json.loads(stdout)
            if isinstance(parsed, dict):
                result = str(parsed.get("result") or parsed.get("content") or "")
                usage = parsed.get("usage")
        except json.JSONDecodeError:
            result = stdout.strip()
    return result.strip(), usage, tool_count, first_content_ms


def extract_generic_result(stdout: str) -> tuple[str, dict[str, Any] | None, int]:
    text = (stdout or "").strip()
    if not text:
        return "", None, 0
    try:
        parsed = json.loads(text)
    except json.JSONDecodeError:
        return text, None, len(re.findall(r"tool|exec|read|write|bash|task", text, re.I))
    if not isinstance(parsed, dict):
        return str(parsed), None, 0
    usage = parsed.get("usage") or parsed.get("meta", {}).get("agentMeta", {}).get("usage")
    tool_count = len(re.findall(r"tool|exec|read|write|bash|task", text, re.I))
    for key in ("result", "text", "content", "message", "output"):
        if key in parsed and parsed[key]:
            return str(parsed[key]).strip(), usage, tool_count
    payloads = parsed.get("payloads")
    if isinstance(payloads, list) and payloads:
        first = payloads[0]
        if isinstance(first, dict):
            return str(first.get("text") or first.get("content") or "").strip(), usage, tool_count
    return text, usage, tool_count


def merge_usage(current: dict[str, Any] | None, new_value: Any) -> dict[str, Any] | None:
    if not isinstance(new_value, dict):
        return current
    merged = dict(current or {})
    for key, value in new_value.items():
        if isinstance(value, dict) and isinstance(merged.get(key), dict):
            nested = dict(merged[key])
            nested.update(value)
            merged[key] = nested
        else:
            merged[key] = value
    return merged


def parse_policy_response(
    protocol: str,
    stream: bool,
    raw_text: str,
) -> tuple[str, dict[str, Any] | None, int, str | None]:
    result_parts: list[str] = []
    usage: dict[str, Any] | None = None
    tool_count = 0
    finish_reason: str | None = None

    if stream:
        for line in raw_text.splitlines():
            stripped = line.strip()
            if not stripped.startswith("data:"):
                continue
            data = stripped[5:].strip()
            if not data or data == "[DONE]":
                continue
            try:
                event = json.loads(data)
            except json.JSONDecodeError:
                continue
            usage = merge_usage(usage, event.get("usage"))
            if protocol == "openai":
                for choice in event.get("choices") or []:
                    delta = choice.get("delta") or {}
                    if delta.get("content"):
                        result_parts.append(str(delta["content"]))
                    if delta.get("tool_calls"):
                        tool_count += len(delta["tool_calls"])
                    if choice.get("finish_reason"):
                        finish_reason = str(choice["finish_reason"])
            else:
                event_type = event.get("type")
                if event_type == "message_start":
                    message = event.get("message") or {}
                    usage = merge_usage(usage, message.get("usage"))
                elif event_type == "content_block_delta":
                    delta = event.get("delta") or {}
                    if delta.get("text"):
                        result_parts.append(str(delta["text"]))
                elif event_type == "content_block_start":
                    block = event.get("content_block") or {}
                    if block.get("type") == "tool_use":
                        tool_count += 1
                elif event_type == "message_delta":
                    usage = merge_usage(usage, event.get("usage"))
                    delta = event.get("delta") or {}
                    if delta.get("stop_reason"):
                        finish_reason = str(delta["stop_reason"])
        return "".join(result_parts).strip(), usage, tool_count, finish_reason

    try:
        parsed = json.loads(raw_text)
    except json.JSONDecodeError:
        return raw_text.strip(), None, 0, None
    if not isinstance(parsed, dict):
        return str(parsed), None, 0, None
    if protocol == "openai":
        usage = parsed.get("usage") if isinstance(parsed.get("usage"), dict) else None
        choices = parsed.get("choices") if isinstance(parsed.get("choices"), list) else []
        for choice in choices:
            message = choice.get("message") or {}
            if message.get("content"):
                result_parts.append(str(message["content"]))
            if message.get("tool_calls"):
                tool_count += len(message["tool_calls"])
            if choice.get("finish_reason"):
                finish_reason = str(choice["finish_reason"])
    else:
        usage = parsed.get("usage") if isinstance(parsed.get("usage"), dict) else None
        for block in parsed.get("content") or []:
            if block.get("type") == "text" and block.get("text"):
                result_parts.append(str(block["text"]))
            if block.get("type") == "tool_use":
                tool_count += 1
        if parsed.get("stop_reason"):
            finish_reason = str(parsed["stop_reason"])
    return "".join(result_parts).strip(), usage, tool_count, finish_reason


def usage_number(usage: dict[str, Any] | None, *keys: str) -> int | None:
    if not isinstance(usage, dict):
        return None
    for key in keys:
        value: Any = usage
        for part in key.split("."):
            if not isinstance(value, dict) or part not in value:
                value = None
                break
            value = value[part]
        if isinstance(value, int):
            return value
    return None


def usage_numbers(protocol: str, usage: dict[str, Any] | None) -> dict[str, int | None]:
    if protocol == "openai":
        input_tokens = usage_number(usage, "prompt_tokens", "input_tokens")
        output_tokens = usage_number(usage, "completion_tokens", "output_tokens")
    else:
        input_tokens = usage_number(usage, "input_tokens", "prompt_tokens")
        output_tokens = usage_number(usage, "output_tokens", "completion_tokens")
    cached_tokens = usage_number(
        usage,
        "prompt_tokens_details.cached_tokens",
        "prompt_cache_hit_tokens",
        "cache_read_input_tokens",
    )
    cache_read = usage_number(
        usage,
        "cache_read_input_tokens",
        "prompt_cache_hit_tokens",
        "prompt_tokens_details.cached_tokens",
    )
    cache_creation = usage_number(usage, "cache_creation_input_tokens")
    cache_miss = usage_number(usage, "cache_miss_input_tokens", "prompt_cache_miss_tokens")
    return {
        "usage_input_tokens": input_tokens,
        "usage_output_tokens": output_tokens,
        "usage_cached_tokens": cached_tokens,
        "usage_cache_read_tokens": cache_read,
        "usage_cache_creation_tokens": cache_creation,
        "usage_cache_miss_tokens": cache_miss,
    }


def classify_cache_observation(
    cache_attempted: bool,
    status_code: int,
    usage: dict[str, Any] | None,
    raw_text: str,
) -> str:
    if not cache_attempted:
        return "ignored"
    numbers = usage_numbers("openai", usage)
    hit_values = [
        numbers.get("usage_cached_tokens"),
        numbers.get("usage_cache_read_tokens"),
    ]
    cache_values = [
        *hit_values,
        numbers.get("usage_cache_creation_tokens"),
        numbers.get("usage_cache_miss_tokens"),
    ]
    present_values = [value for value in cache_values if value is not None]
    if any((value or 0) > 0 for value in hit_values if value is not None):
        return "accepted"
    lower = (raw_text or "").lower()
    if status_code in {400, 422} or (
        status_code >= 400 and ("cache" in lower or "prompt caching" in lower)
    ):
        return "rejected"
    if present_values:
        return "attempted"
    return "ignored"


def policy_error_class(status_code: int, raw_text: str) -> str:
    classified = classify_api_error_text(raw_text, status_code)
    if classified:
        return classified
    if 200 <= status_code < 300:
        return "ok"
    if status_code == 0:
        return "network_error"
    return "unknown_error"


def policy_case_ok(case: PolicyCaseSpec, row: dict[str, Any]) -> bool:
    if not row.get("redaction_ok"):
        return False
    if case.cache_attempted:
        if row.get("error_class") in {"auth_error", "model_error", "network_error"}:
            return False
        return row.get("cache_observation") in {"attempted", "accepted", "rejected", "ignored"}
    if not row.get("api_ok"):
        return False
    if case.case_type == "flash_input_room":
        return row.get("input_wall_ok") is True and "FLASH_INPUT_OK" in row.get("result_prefix", "")
    if case.case_type == "flash_output_room":
        return row.get("output_wall_ok") is True and "FLASH_OUTPUT_OK" in row.get("result_prefix", "")
    if case.case_type == "lite_not_claudecode":
        return (
            row.get("expected_effective_client") == "unknown"
            and "LITE_POLICY_OK" in row.get("result_prefix", "")
        )
    if case.case_type == "provider_usage_probe":
        return row.get("provider_body_usage_signal") is True and "USAGE_POLICY_OK" in row.get("result_prefix", "")
    return True


def model_for_index(models: list[str], idx: int) -> str:
    return models[idx % len(models)]


def no_proxy_for_base_url(base_url: str) -> str:
    parsed = urlparse(base_url)
    host = parsed.hostname or ""
    entries = ["127.0.0.1", "localhost", "::1"]
    if host:
        entries.insert(0, host)
    if host.startswith("100."):
        entries.extend(["100.64.0.0/10", "panda"])
    seen: set[str] = set()
    return ",".join(entry for entry in entries if entry and not (entry in seen or seen.add(entry)))


def claude_settings_json(base_url: str, model: str, api_key: str | None = None) -> str:
    env = {
        "ANTHROPIC_BASE_URL": base_url,
        "ANTHROPIC_MODEL": model,
        "ANTHROPIC_SMALL_FAST_MODEL": model,
        "ANTHROPIC_DEFAULT_HAIKU_MODEL": model,
        "ANTHROPIC_DEFAULT_HAIKU_MODEL_NAME": model,
        "ANTHROPIC_DEFAULT_OPUS_MODEL": model,
        "ANTHROPIC_DEFAULT_OPUS_MODEL_NAME": model,
        "ANTHROPIC_DEFAULT_SONNET_MODEL": model,
        "ANTHROPIC_DEFAULT_SONNET_MODEL_NAME": model,
        "NO_PROXY": no_proxy_for_base_url(base_url),
        "no_proxy": no_proxy_for_base_url(base_url),
    }
    if api_key:
        # ClaudeCode may otherwise keep a stale settings-layer x-api-key such as
        # sk-dev even when ANTHROPIC_AUTH_TOKEN is correct in the process env.
        env["ANTHROPIC_API_KEY"] = api_key
        env["ANTHROPIC_AUTH_TOKEN"] = api_key
    return json.dumps(
        {"env": env},
        separators=(",", ":"),
    )


def claude_command(
    case: CaseSpec,
    model: str,
    base_url: str,
    windows: bool = False,
    include_settings: bool = True,
    api_key: str | None = None,
) -> list[str]:
    claude_bin = os.environ.get("PANDA_WSL_CLAUDE_BIN", DEFAULT_WSL_CLAUDE_BIN)
    if not windows and os.name != "nt" and not Path(claude_bin).exists():
        claude_bin = "/home/lenovo/.local/bin/claude"
    base = [
        "claude" if windows else claude_bin,
        "-p",
        "--bare",
        "--model",
        model,
        "--output-format",
        "stream-json",
        "--verbose",
        "--include-partial-messages",
        "--no-session-persistence",
        "--dangerously-skip-permissions",
    ]
    if include_settings:
        base[3:3] = [
            "--setting-sources",
            "",
            "--settings",
            claude_settings_json(base_url, model, api_key),
        ]
    if case.tools:
        base.extend(
            [
                "--tools",
                "Read,Bash,Task",
                "--allowedTools",
                "Read,Task,Bash(cat:*),Bash(awk:*),Bash(python3:*)",
            ]
        )
    return base


def run_wsl_claudecode(
    case: CaseSpec,
    model: str,
    prompt_text: str,
    workspace: Path,
    base_url: str,
    key: str,
    timeout_ms: int,
) -> dict[str, Any]:
    env = {
        "ANTHROPIC_BASE_URL": base_url,
        "ANTHROPIC_AUTH_TOKEN": key,
        "ANTHROPIC_API_KEY": key,
        "ANTHROPIC_MODEL": model,
        "ANTHROPIC_SMALL_FAST_MODEL": model,
        "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC": "1",
        "NO_PROXY": no_proxy_for_base_url(base_url),
        "no_proxy": no_proxy_for_base_url(base_url),
    }
    if os.name == "nt":
        args = claude_command(case, model, base_url, api_key=key)
        timeout_s = max(1, timeout_ms // 1000)
        wsl_claude_bin = shlex.quote(args[0])
        script = (
            f"cd {shlex.quote(path_for_wsl(workspace))} && "
            f"[ -x {wsl_claude_bin} ] || "
            f"(echo 'WSL official ClaudeCode binary not found at {args[0]}' >&2; exit 127) && "
            f"export CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1 && "
            f"export ANTHROPIC_AUTH_TOKEN={shlex.quote(key)} && "
            f"export ANTHROPIC_API_KEY={shlex.quote(key)} && "
            f"export ANTHROPIC_BASE_URL={shlex.quote(base_url)} && "
            f"export ANTHROPIC_MODEL={shlex.quote(model)} && "
            f"export ANTHROPIC_SMALL_FAST_MODEL={shlex.quote(model)} && "
            f"export NO_PROXY={shlex.quote(no_proxy_for_base_url(base_url))} && "
            f"export no_proxy={shlex.quote(no_proxy_for_base_url(base_url))} && "
            f"cat | timeout {timeout_s}s "
            + " ".join([wsl_claude_bin, *[shlex.quote(arg) for arg in args[1:]]])
        )
        rec = run_process(
            ["wsl", "-d", "HermesUbuntu", "-u", "lenovo", "bash", "-lc", script],
            workspace,
            env,
            timeout_ms + 5000,
            prompt_text,
        )
        result, usage, tool_count, first_content_offset = extract_claude_stream(rec.get("stdout", ""))
        rec.update(
            result=result,
            usage=usage,
            tool_call_count=tool_count,
            first_content_ms=rec.get("first_stdout_ms") if first_content_offset is not None else rec.get("first_stdout_ms"),
            config_mode="wsl-interop-env-settings",
        )
        return rec
    rec = run_process(
        claude_command(case, model, base_url, api_key=key),
        workspace,
        env,
        timeout_ms,
        prompt_text,
    )
    result, usage, tool_count, first_content_offset = extract_claude_stream(rec.get("stdout", ""))
    rec.update(
        result=result,
        usage=usage,
        tool_call_count=tool_count,
        first_content_ms=rec.get("first_stdout_ms") if first_content_offset is not None else rec.get("first_stdout_ms"),
        config_mode="env-settings",
    )
    return rec


def powershell_quote(value: str) -> str:
    return "'" + value.replace("'", "''") + "'"


def powershell_env_assignments(env: dict[str, str]) -> str:
    return "; ".join(f"$env:{key} = {powershell_quote(value)}" for key, value in env.items())


def wsl_to_windows_path(path: Path) -> str:
    try:
        proc = subprocess.run(
            ["wslpath", "-w", str(path)],
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=True,
        )
        return proc.stdout.strip()
    except Exception:
        return str(path)


def path_for_wsl(path: Path) -> str:
    text = str(path)
    prefixes = (
        "\\\\wsl.localhost\\HermesUbuntu\\",
        "\\\\wsl$\\HermesUbuntu\\",
    )
    for prefix in prefixes:
        if text.startswith(prefix):
            suffix = text[len(prefix) :].replace("\\", "/")
            return "/" + suffix.lstrip("/")
    return text.replace("\\", "/")


def windows_temp_dir_pair() -> tuple[Path, PureWindowsPath]:
    if os.name == "nt":
        temp = Path(os.environ.get("TEMP") or tempfile.gettempdir())
        return temp, PureWindowsPath(str(temp))

    win_temp = ""
    for cmd in (
        ["cmd.exe", "/c", "echo %TEMP%"],
        ["powershell.exe", "-NoProfile", "-Command", "[System.IO.Path]::GetTempPath()"],
    ):
        try:
            proc = subprocess.run(
                cmd,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                check=True,
            )
            win_temp = proc.stdout.decode("utf-8", errors="ignore").strip()
            if win_temp:
                break
        except (OSError, subprocess.CalledProcessError):
            continue
    if not win_temp:
        win_temp = r"C:\Users\Lenovo\AppData\Local\Temp"
    try:
        wsl_proc = subprocess.run(
            ["wslpath", "-u", win_temp],
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=True,
        )
        wsl_temp = Path(wsl_proc.stdout.strip())
    except (OSError, subprocess.CalledProcessError):
        wsl_temp = Path("/mnt/c/Users/Lenovo/AppData/Local/Temp")
    return wsl_temp, PureWindowsPath(win_temp)


def copy_workspace_for_windows(
    workspace: Path,
    run_dir: Path,
) -> tuple[Path, Path | PureWindowsPath]:
    """Return a Windows-local workspace and prompt path root for ClaudeCode.

    ClaudeCode on Windows cannot reliably run from a ``\\wsl.localhost`` cwd.
    Keep the runner output under the repo, but run Windows ClaudeCode from a
    local TEMP workspace with Windows paths in the prompt.
    """

    key = f"{workspace.resolve()}::{run_dir.name}"
    with _WINDOWS_WORKSPACE_LOCK:
        cached = _WINDOWS_WORKSPACE_CACHE.get(key)
        if cached:
            return cached
        temp_root, prompt_root = windows_temp_dir_pair()
        local_root = temp_root / "zenproxy-panda-pressure" / run_dir.name
        local_workspace = local_root / "workspace"
        try:
            if local_workspace.exists():
                shutil.rmtree(local_workspace)
            copy_tree_contents(workspace, local_workspace)
            prompt_workspace = prompt_root / "zenproxy-panda-pressure" / run_dir.name / "workspace"
        except OSError:
            local_workspace = workspace
            prompt_workspace = workspace
        cached = (local_workspace, prompt_workspace)
        _WINDOWS_WORKSPACE_CACHE[key] = cached
        return cached


def copy_tree_contents(src: Path, dst: Path) -> None:
    """Copy file bytes only; drvfs can reject metadata preservation."""

    dst.mkdir(parents=True, exist_ok=True)
    for item in src.iterdir():
        target = dst / item.name
        if item.is_dir():
            copy_tree_contents(item, target)
        elif item.is_file():
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(item, target)


def windows_claude_available() -> bool:
    if os.name == "nt":
        return shutil.which("claude") is not None
    # This works only when WSL interop is enabled. Some panda/WSL sessions have
    # Windows exe launching disabled, so classify that as a config error instead
    # of a model or provider failure.
    probe = subprocess.run(
        [
            "bash",
            "-lc",
            "powershell.exe -NoProfile -Command "
            + shlex.quote(
                "if (Get-Command claude -ErrorAction SilentlyContinue) { exit 0 } else { exit 7 }"
            ),
        ],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    return probe.returncode == 0


def run_windows_claudecode(
    case: CaseSpec,
    model: str,
    prompt_text: str,
    workspace: Path,
    base_url: str,
    key: str,
    timeout_ms: int,
) -> dict[str, Any]:
    env = {
        "ANTHROPIC_BASE_URL": base_url,
        "ANTHROPIC_AUTH_TOKEN": key,
        "ANTHROPIC_API_KEY": key,
        "ANTHROPIC_MODEL": model,
        "ANTHROPIC_SMALL_FAST_MODEL": model,
        "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC": "1",
        "NO_PROXY": no_proxy_for_base_url(base_url),
        "no_proxy": no_proxy_for_base_url(base_url),
    }
    if os.name == "nt":
        claude = shutil.which("claude")
        if not claude:
            return {
                "ok": False,
                "returncode": 127,
                "timed_out": False,
                "total_ms": 0,
                "stdout": "",
                "stderr": "Windows claude command not found in PATH",
                "error_class": "config_error",
                "config_mode": "windows-native",
            }
        # Windows ClaudeCode rejects empty --setting-sources; env carries base_url/key.
        cmd = claude_command(case, model, base_url, windows=True, include_settings=False, api_key=key)
        cmd[0] = claude
        rec = run_process(cmd, workspace, env, timeout_ms, prompt_text)
        result, usage, tool_count, first_content_offset = extract_claude_stream(rec.get("stdout", ""))
        rec.update(
            result=result,
            usage=usage,
            tool_call_count=tool_count,
            first_content_ms=rec.get("first_stdout_ms") if first_content_offset is not None else rec.get("first_stdout_ms"),
            config_mode="windows-native-env-settings",
        )
        return rec
    if shutil.which("powershell.exe") is None:
        return {
            "ok": False,
            "returncode": 127,
            "timed_out": False,
            "total_ms": 0,
            "stdout": "",
            "stderr": "powershell.exe not available from WSL",
            "error_class": "config_error",
            "config_mode": "windows-interop",
        }
    if not windows_claude_available():
        return {
            "ok": False,
            "returncode": 127,
            "timed_out": False,
            "total_ms": 0,
            "stdout": "",
            "stderr": "Windows ClaudeCode unavailable from this WSL session. Either WSL interop is disabled or claude is not in the non-interactive PowerShell PATH.",
            "error_class": "config_error",
            "config_mode": "windows-interop",
        }
    prompt_path = workspace / f"prompt-{uuid.uuid4().hex}.txt"
    prompt_path.write_text(prompt_text, encoding="utf-8")
    win_prompt = wsl_to_windows_path(prompt_path)
    # Windows ClaudeCode rejects empty --setting-sources; env carries base_url/key.
    args = claude_command(case, model, base_url, windows=True, include_settings=False, api_key=key)
    ps_args = " ".join(powershell_quote(arg) for arg in args[1:])
    ps = (
        powershell_env_assignments(env)
        + "; Get-Content -Raw -LiteralPath "
        + powershell_quote(win_prompt)
        + " | claude "
        + ps_args
    )
    rec = run_process(
        [
            "bash",
            "-lc",
            "powershell.exe -NoProfile -ExecutionPolicy Bypass -Command " + shlex.quote(ps),
        ],
        workspace,
        env,
        timeout_ms,
    )
    result, usage, tool_count, first_content_offset = extract_claude_stream(rec.get("stdout", ""))
    rec.update(
        result=result,
        usage=usage,
        tool_call_count=tool_count,
        first_content_ms=rec.get("first_stdout_ms") if first_content_offset is not None else rec.get("first_stdout_ms"),
        config_mode="windows-interop-env-settings",
    )
    return rec


def run_hermes(
    case: CaseSpec,
    model: str,
    prompt_text: str,
    workspace: Path,
    base_url: str,
    key: str,
    timeout_ms: int,
    run_dir: Path,
) -> dict[str, Any]:
    hermes = shutil.which("hermes") or "/home/lenovo/.local/bin/hermes"
    if not Path(hermes).exists():
        return {
            "ok": False,
            "returncode": 127,
            "timed_out": False,
            "total_ms": 0,
            "stdout": "",
            "stderr": "hermes command not found",
            "error_class": "config_error",
            "config_mode": "temp-home",
        }
    home = run_dir / "hermes_home"
    home.mkdir(parents=True, exist_ok=True)
    effective_prompt = prompt_text
    effective_tools = case.tools
    if len(prompt_text.encode("utf-8")) > 100_000:
        effective_prompt = file_backed_prompt(prompt_text, workspace, "hermes", case.case_type)
        effective_tools = True
    cmd = [
        hermes,
        "--provider",
        "custom",
        "--model",
        model,
        "-z",
        effective_prompt,
        "--ignore-user-config",
        "--ignore-rules",
        "--accept-hooks",
    ]
    if effective_tools:
        cmd.extend(["--toolsets", "hermes-cli"])
    env = {
        "HERMES_HOME": str(home),
        "CUSTOM_BASE_URL": url_join(base_url, "/v1"),
        "OPENAI_API_KEY": key,
        "NEWAPI_API_KEY": key,
    }
    rec = run_process(cmd, workspace, env, timeout_ms)
    result, usage, tool_count = extract_generic_result(rec.get("stdout", ""))
    rec.update(
        result=result,
        usage=usage,
        tool_call_count=tool_count,
        first_content_ms=rec.get("first_stdout_ms"),
        config_mode="temp-home-custom-provider",
    )
    return rec


def write_openclaw_config(path: Path, workspace: Path, base_url: str, models: list[str]) -> None:
    provider_models = []
    for model in models:
        provider_models.append(
            {
                "id": model,
                "name": f"{model} (Panda NewAPI)",
                "contextWindow": 1000000,
                "maxTokens": 4096,
                "input": ["text"],
                "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0},
                "reasoning": False,
            }
        )
    cfg = {
        "agents": {
            "defaults": {
                "workspace": str(workspace),
                "model": {"primary": f"zenproxy/{models[0]}"},
                "models": {f"zenproxy/{model}": {"alias": model} for model in models},
            }
        },
        "tools": {"profile": "coding"},
        "models": {
            "mode": "merge",
            "providers": {
                "zenproxy": {
                    "baseUrl": url_join(base_url, "/v1"),
                    "api": "openai-completions",
                    "apiKey": {
                        "source": "env",
                        "provider": "default",
                        "id": "NEWAPI_API_KEY",
                    },
                    "models": provider_models,
                }
            },
        },
    }
    safe_write_json(path, cfg)


def run_openclaw(
    case: CaseSpec,
    model: str,
    prompt_text: str,
    workspace: Path,
    base_url: str,
    key: str,
    timeout_ms: int,
    run_dir: Path,
    models: list[str],
) -> dict[str, Any]:
    openclaw = Path("/home/lenovo/.local/node_modules/.bin/openclaw")
    if not openclaw.exists():
        found = shutil.which("openclaw")
        openclaw = Path(found) if found else openclaw
    if not openclaw.exists():
        return {
            "ok": False,
            "returncode": 127,
            "timed_out": False,
            "total_ms": 0,
            "stdout": "",
            "stderr": "openclaw command not found",
            "error_class": "config_error",
            "config_mode": "temp-config",
        }
    home = run_dir / "openclaw_home"
    home.mkdir(parents=True, exist_ok=True)
    cfg = home / "openclaw.json"
    if not cfg.exists():
        write_openclaw_config(cfg, workspace, base_url, models)
    node22 = Path("/home/lenovo/.local/opt/node-v22.21.1-linux-x64/bin")
    path_parts = []
    if node22.exists():
        path_parts.append(str(node22))
    path_parts.extend(
        [
            "/home/lenovo/.local/node_modules/.bin",
            "/usr/local/sbin",
            "/usr/local/bin",
            "/usr/sbin",
            "/usr/bin",
            "/sbin",
            "/bin",
        ]
    )
    env = {
        "PATH": ":".join(path_parts),
        "OPENCLAW_CONFIG_PATH": str(cfg),
        "OPENCLAW_HOME": str(home),
        "OPENCLAW_STATE_DIR": str(home / "state"),
        "NEWAPI_API_KEY": key,
    }
    effective_prompt = prompt_text
    if len(prompt_text.encode("utf-8")) > 100_000:
        effective_prompt = file_backed_prompt(prompt_text, workspace, "openclaw", case.case_type)
    cmd = [
        str(openclaw),
        "agent",
        "--local",
        "--json",
        "--session-key",
        f"agent:panda-pressure:{uuid.uuid4().hex}",
        "--model",
        f"zenproxy/{model}",
        "--timeout",
        str(max(1, timeout_ms // 1000)),
        "--message",
        effective_prompt,
    ]
    rec = run_process(cmd, workspace, env, timeout_ms + 30000)
    result, usage, tool_count = extract_generic_result(rec.get("stdout", ""))
    rec.update(
        result=result,
        usage=usage,
        tool_call_count=tool_count,
        first_content_ms=rec.get("first_stdout_ms"),
        config_mode="temp-config",
    )
    return rec


def run_case(
    client: str,
    case: CaseSpec,
    idx: int,
    model: str,
    workspace: Path,
    base_url: str,
    key: str,
    timeout_ms: int,
    run_dir: Path,
    models: list[str],
) -> dict[str, Any]:
    request_id = f"{client}-{idx:04d}-{uuid.uuid4().hex[:8]}"
    case_workspace = workspace
    prompt_workspace: Path | PureWindowsPath | None = None
    if client == "windows-claudecode":
        case_workspace, prompt_workspace = copy_workspace_for_windows(workspace, run_dir)
    prompt_text = build_prompt(case, case_workspace, prompt_workspace)
    prompt_bytes = len(prompt_text.encode("utf-8"))
    prompt_tokens = estimate_tokens(prompt_text)
    started = time.time()
    base_row: dict[str, Any] = {
        "run_id": run_dir.name,
        "request_id": request_id,
        "timestamp": started,
        "client": client,
        "host": platform.node(),
        "base_url_kind": base_url_kind(base_url),
        "model": model,
        "protocol": "client-cli",
        "stream": case.stream,
        "case_type": case.case_type,
        "prompt_est_tokens": prompt_tokens,
        "prompt_bytes": prompt_bytes,
        "prompt_sha256": sha256_text(prompt_text),
        "output_est_tokens": None,
        "response_bytes": None,
        "status": "started",
        "api_ok": None,
        "status_code": None,
        "error_class": None,
        "retry_count": 0,
        "timeout_ms": timeout_ms,
        "protocol_first_byte_ms": None,
        "first_content_ms": None,
        "first_tool_call_ms": None,
        "first_tool_emit_ms": None,
        "total_ms": None,
        "tool_call_count": 0,
        "tool_success": None,
        "subagent_requested": case.subagent_requested,
        "subagent_observed": None,
        "config_mode": None,
        "redaction_ok": False,
        "semantic_ok": False,
        "result_prefix": "",
        "stderr_prefix": "",
    }
    base_row.update(prompt_observation_fields(prompt_text, prompt_tokens, prompt_tokens))
    try:
        if client == "wsl-claudecode":
            rec = run_wsl_claudecode(case, model, prompt_text, case_workspace, base_url, key, timeout_ms)
        elif client == "windows-claudecode":
            rec = run_windows_claudecode(case, model, prompt_text, case_workspace, base_url, key, timeout_ms)
        elif client == "wsl-hermes":
            rec = run_hermes(case, model, prompt_text, case_workspace, base_url, key, timeout_ms, run_dir)
        elif client == "wsl-openclaw":
            rec = run_openclaw(case, model, prompt_text, case_workspace, base_url, key, timeout_ms, run_dir, models)
        else:
            rec = {
                "ok": False,
                "returncode": 2,
                "timed_out": False,
                "total_ms": 0,
                "stdout": "",
                "stderr": f"unsupported client {client}",
                "error_class": "config_error",
            }
    except Exception as exc:  # noqa: BLE001 - runner must keep going.
        rec = {
            "ok": False,
            "returncode": 1,
            "timed_out": False,
            "total_ms": 0,
            "stdout": "",
            "stderr": f"runner_exception:{type(exc).__name__}:{exc}",
            "error_class": "unknown_error",
        }
    result = str(rec.get("result") or "")
    semantic_ok = expected_semantic_ok(case, result)
    embedded_failure = classify_embedded_failure(result, rec) if rec.get("ok") else None
    api_ok = bool(rec.get("ok")) and embedded_failure is None
    if not rec.get("ok"):
        status = "error"
        error_class = rec.get("error_class") or classify_process_error(rec)
    elif embedded_failure:
        status = "error"
        error_class = embedded_failure
    elif not semantic_ok:
        status = "error"
        error_class = classify_semantic_failure(client, case, result)
    else:
        status = "ok"
        error_class = "ok"
    response_bytes = len(result.encode("utf-8", errors="ignore"))
    usage_values = usage_numbers("openai", rec.get("usage"))
    row = dict(base_row)
    row.update(
        status=status,
        api_ok=api_ok,
        returncode=rec.get("returncode"),
        error_class=error_class,
        protocol_first_byte_ms=rec.get("first_stdout_ms"),
        first_content_ms=rec.get("first_content_ms") or rec.get("first_stdout_ms"),
        first_tool_call_ms=rec.get("first_tool_call_ms"),
        first_tool_emit_ms=rec.get("first_tool_emit_ms"),
        total_ms=rec.get("total_ms"),
        tool_call_count=rec.get("tool_call_count") or 0,
        tool_success=(semantic_ok if case.tools else None),
        subagent_supported=(client in SUBAGENT_CAPABLE_CLIENTS if case.subagent_requested else None),
        subagent_observed=detect_subagent_observed(client, case, rec, semantic_ok),
        config_mode=rec.get("config_mode"),
        semantic_ok=semantic_ok,
        usage=rec.get("usage"),
        output_est_tokens=estimate_tokens(result) if result else 0,
        response_bytes=response_bytes,
        result_prefix=result[:RESULT_PREFIX_LIMIT],
        stderr_prefix=(rec.get("stderr") or "")[-STDERR_PREFIX_LIMIT:],
    )
    row.update(usage_values)
    row.update(cache_token_fields(usage_values, prompt_tokens))
    serialized = json.dumps(row, ensure_ascii=False)
    row["redaction_ok"] = key not in serialized and not any(
        os.environ.get(name, "") and os.environ.get(name, "") in serialized for name in KEY_ENV_NAMES
    )
    return row


def summarize(rows: list[dict[str, Any]]) -> dict[str, Any]:
    summary: dict[str, Any] = {
        "total": len(rows),
        "ok": sum(1 for row in rows if row.get("status") == "ok"),
        "api_ok": sum(1 for row in rows if row.get("api_ok")),
        "semantic_ok": sum(1 for row in rows if row.get("semantic_ok")),
        "redaction_ok": all(row.get("redaction_ok") for row in rows),
        "by_client": {},
        "by_error_class": {},
        "failure_samples": [],
    }
    for row in rows:
        error_class = row.get("error_class") or "unknown_error"
        summary["by_error_class"][error_class] = summary["by_error_class"].get(error_class, 0) + 1
    for client in sorted({str(row.get("client")) for row in rows}):
        client_rows = [row for row in rows if row.get("client") == client]
        totals = [row.get("total_ms") for row in client_rows if isinstance(row.get("total_ms"), int)]
        firsts = [
            row.get("first_content_ms")
            for row in client_rows
            if isinstance(row.get("first_content_ms"), int)
        ]
        tool_rows = [row for row in client_rows if row.get("tool_success") is not None]
        sub_rows = [
            row
            for row in client_rows
            if row.get("subagent_requested") and row.get("subagent_supported") is not False
        ]
        sub_not_supported = [
            row
            for row in client_rows
            if row.get("subagent_requested") and row.get("subagent_supported") is False
        ]
        summary["by_client"][client] = {
            "total": len(client_rows),
            "ok": sum(1 for row in client_rows if row.get("status") == "ok"),
            "api_ok": sum(1 for row in client_rows if row.get("api_ok")),
            "semantic_ok": sum(1 for row in client_rows if row.get("semantic_ok")),
            "p50_total_ms": percentile(totals, 50),
            "p90_total_ms": percentile(totals, 90),
            "p95_total_ms": percentile(totals, 95),
            "p99_total_ms": percentile(totals, 99),
            "p50_first_content_ms": percentile(firsts, 50),
            "p90_first_content_ms": percentile(firsts, 90),
            "p95_first_content_ms": percentile(firsts, 95),
            "p99_first_content_ms": percentile(firsts, 99),
            "latency_ms": latency_percentile_map(client_rows),
            "tool_total": len(tool_rows),
            "tool_success": sum(1 for row in tool_rows if row.get("tool_success")),
            "subagent_total": len(sub_rows),
            "subagent_not_supported": len(sub_not_supported),
            "subagent_observed": sum(1 for row in sub_rows if row.get("subagent_observed")),
            "errors": {},
        }
        for row in client_rows:
            error_class = row.get("error_class") or "unknown_error"
            target = summary["by_client"][client]["errors"]
            target[error_class] = target.get(error_class, 0) + 1
    summary["observability"] = observability_summary(rows)
    summary["failure_samples"] = [
        {
            "request_id": row.get("request_id"),
            "client": row.get("client"),
            "case_type": row.get("case_type"),
            "model": row.get("model"),
            "status": row.get("status"),
            "semantic_ok": row.get("semantic_ok"),
            "error_class": row.get("error_class"),
            "total_ms": row.get("total_ms"),
            "stderr_prefix": row.get("stderr_prefix"),
            "result_prefix": row.get("result_prefix"),
        }
        for row in rows
        if row.get("status") != "ok" or not row.get("semantic_ok")
    ][:20]
    return summary


def preflight(base_url: str, key: str, models: list[str], timeout_s: int) -> dict[str, Any]:
    status, parsed, raw, total_ms, first_ms = http_json(
        "GET", url_join(base_url, "/v1/models"), key, timeout_s=timeout_s
    )
    model_ids: list[str] = []
    if isinstance(parsed, dict):
        data = parsed.get("data")
        if isinstance(data, list):
            model_ids = [str(item.get("id")) for item in data if isinstance(item, dict)]
    missing = [model for model in models if model not in model_ids]
    chat_payload = {
        "model": models[0],
        "stream": False,
        "max_tokens": 16,
        "messages": [{"role": "user", "content": "Reply exactly OK."}],
    }
    chat_status, chat_parsed, chat_raw, chat_total, chat_first = http_json(
        "POST",
        url_join(base_url, "/v1/chat/completions"),
        key,
        payload=chat_payload,
        timeout_s=max(timeout_s, 60),
        extra_headers={"x-fmc-client": "claude-code"},
    )
    content = ""
    if isinstance(chat_parsed, dict):
        try:
            content = str(chat_parsed["choices"][0]["message"].get("content") or "")
        except Exception:
            content = ""
    models_error_class = classify_api_error_text(raw, status)
    chat_error_class = classify_api_error_text(chat_raw, chat_status)
    error_class = models_error_class or chat_error_class
    if status == 200 and missing and not error_class:
        error_class = "model_error"
    chat_ok = chat_status == 200 and "ok" in content.lower()
    ok = status == 200 and not missing and chat_ok
    if not ok and not error_class:
        error_class = "preflight_failed"
    blocker = None
    if not ok:
        if error_class == "auth_error":
            blocker = "invalid_or_missing_newapi_token"
        elif error_class == "channel_unavailable":
            blocker = "newapi_channel_unavailable"
        elif missing:
            blocker = "target_models_missing_from_newapi"
        elif chat_status != 200:
            blocker = "minimal_chat_failed"
        else:
            blocker = "minimal_chat_semantic_failed"
    return {
        "models_status": status,
        "models_total_ms": total_ms,
        "models_first_byte_ms": first_ms,
        "model_count": len(model_ids),
        "model_sample": model_ids[:20],
        "missing_models": missing,
        "chat_status": chat_status,
        "chat_total_ms": chat_total,
        "chat_first_byte_ms": chat_first,
        "chat_content_prefix": content[:RESULT_PREFIX_LIMIT] or chat_raw[:RESULT_PREFIX_LIMIT],
        "error_class": error_class,
        "blocker": blocker,
        "ok": ok,
        "redaction_ok": key not in raw and key not in chat_raw,
    }


def run_policy_case(
    case: PolicyCaseSpec,
    idx: int,
    base_url: str,
    key: str,
    timeout_ms: int,
    run_dir: Path,
) -> dict[str, Any]:
    request_id = f"policy-{case.protocol}-{idx:04d}-{uuid.uuid4().hex[:8]}"
    prompt = build_policy_prompt(case)
    path, payload, shape_messages, tool_names = build_policy_payload(case, prompt)
    shape = policy_shape_fields(case, shape_messages, tool_names)
    request_body_bytes = len(json_compact(payload).encode("utf-8"))
    prompt_bytes = len(prompt.encode("utf-8"))
    started = time.time()
    headers = {"x-fmc-client": case.client_header}
    exchange = http_exchange(
        "POST",
        url_join(base_url, path),
        key,
        payload,
        timeout_s=max(1, timeout_ms // 1000),
        extra_headers=headers,
    )
    raw_text = str(exchange.get("raw_text") or "")
    status_code = int(exchange.get("status_code") or 0)
    result, usage, tool_count, finish_reason = parse_policy_response(
        case.protocol,
        case.stream,
        raw_text,
    )
    usage_values = usage_numbers(case.protocol, usage)
    output_est_tokens = estimate_tokens(result) if result else 0
    body_usage_signal = usage_values["usage_input_tokens"] is not None or usage_values["usage_output_tokens"] is not None
    provider_header_names = sorted((exchange.get("headers") or {}).keys())
    error_class = policy_error_class(status_code, raw_text)
    api_ok = 200 <= status_code < 300 and error_class == "ok"
    cache_observation = classify_cache_observation(
        case.cache_attempted,
        status_code,
        usage,
        raw_text,
    )
    input_wall_ok = None
    output_wall_ok = None
    if case.case_type == "flash_input_room":
        input_wall_ok = (
            api_ok
            and status_code not in {400, 413, 422}
            and prompt_bytes >= case.prompt_target_tokens * 3
        )
    if case.case_type == "flash_output_room":
        output_wall_ok = (
            api_ok
            and output_est_tokens >= case.expected_min_output_tokens
            and finish_reason not in {"length", "max_tokens"}
        )
    first_content_ms = exchange.get("first_content_ms")
    if first_content_ms is None and result:
        first_content_ms = exchange.get("protocol_first_byte_ms") if case.stream else exchange.get("total_ms")
    first_tool_call_ms = exchange.get("first_tool_call_ms")
    if first_tool_call_ms is None and tool_count:
        first_tool_call_ms = exchange.get("protocol_first_byte_ms") if case.stream else exchange.get("total_ms")
    first_tool_emit_ms = exchange.get("first_tool_emit_ms") or first_tool_call_ms
    row: dict[str, Any] = {
        "run_id": run_dir.name,
        "request_id": request_id,
        "timestamp": started,
        "client": "direct-http",
        "host": platform.node(),
        "base_url_kind": base_url_kind(base_url),
        "protocol": case.protocol,
        "endpoint": path,
        "model": case.model,
        "stream": case.stream,
        "case_type": case.case_type,
        "x_fmc_client": case.client_header,
        "expected_source_client": case.expected_source_client,
        "expected_effective_client": case.expected_effective_client,
        "profile_source_expected": "header",
        "prompt_est_tokens": estimate_tokens(prompt),
        "prompt_target_tokens": case.prompt_target_tokens or None,
        "prompt_bytes": prompt_bytes,
        "prompt_sha256": sha256_text(prompt),
        "request_body_bytes": request_body_bytes,
        "max_tokens": case.max_tokens,
        "expected_min_output_tokens": case.expected_min_output_tokens or None,
        "status": "ok" if api_ok else "error",
        "api_ok": api_ok,
        "status_code": status_code,
        "error_class": error_class,
        "retry_count": 0,
        "timeout_ms": timeout_ms,
        "protocol_first_byte_ms": exchange.get("protocol_first_byte_ms"),
        "first_content_ms": first_content_ms,
        "first_tool_call_ms": first_tool_call_ms,
        "first_tool_emit_ms": first_tool_emit_ms,
        "total_ms": exchange.get("total_ms"),
        "tool_call_count": tool_count,
        "tool_success": None,
        "subagent_requested": False,
        "subagent_supported": None,
        "subagent_observed": None,
        "config_mode": "direct-http-temp-headers",
        "semantic_ok": None,
        "output_est_tokens": output_est_tokens,
        "response_bytes": len(result.encode("utf-8", errors="ignore")),
        "finish_reason": finish_reason,
        "provider_header_signal": bool(provider_header_names),
        "provider_header_names": provider_header_names,
        "provider_body_usage_signal": body_usage_signal,
        "usage": usage,
        "cache_attempted": case.cache_attempted,
        "cache_observation": cache_observation,
        "input_wall_ok": input_wall_ok,
        "output_wall_ok": output_wall_ok,
        "result_prefix": result[:RESULT_PREFIX_LIMIT],
        "stderr_prefix": raw_text[:STDERR_PREFIX_LIMIT] if not api_ok else "",
    }
    row.update(prompt_observation_fields(prompt, estimate_tokens(prompt), case.prompt_target_tokens or estimate_tokens(prompt)))
    row.update(shape)
    row.update(usage_values)
    row.update(cache_token_fields(usage_values, estimate_tokens(prompt)))
    serialized = json.dumps(row, ensure_ascii=False)
    row["redaction_ok"] = key not in serialized and not any(
        os.environ.get(name, "") and os.environ.get(name, "") in serialized for name in KEY_ENV_NAMES
    )
    row["policy_ok"] = policy_case_ok(case, row)
    row["status"] = "ok" if row["policy_ok"] else row["status"]
    return row


def summarize_policy(rows: list[dict[str, Any]], require_provider_header: bool) -> dict[str, Any]:
    summary: dict[str, Any] = {
        "total": len(rows),
        "ok": sum(1 for row in rows if row.get("policy_ok")),
        "policy_ok": sum(1 for row in rows if row.get("policy_ok")),
        "api_ok": sum(1 for row in rows if row.get("api_ok")),
        "redaction_ok": all(row.get("redaction_ok") for row in rows),
        "provider_header_signal_rows": sum(1 for row in rows if row.get("provider_header_signal")),
        "provider_body_usage_signal_rows": sum(1 for row in rows if row.get("provider_body_usage_signal")),
        "require_provider_header": require_provider_header,
        "by_protocol": {},
        "by_case_type": {},
        "by_cache_observation": {},
        "by_error_class": {},
        "wall_failures": [],
        "failure_samples": [],
    }
    for row in rows:
        for key, field in [
            ("by_protocol", "protocol"),
            ("by_case_type", "case_type"),
            ("by_cache_observation", "cache_observation"),
            ("by_error_class", "error_class"),
        ]:
            value = str(row.get(field) or "unknown")
            target = summary[key]
            target[value] = target.get(value, 0) + 1
        if row.get("input_wall_ok") is False or row.get("output_wall_ok") is False:
            summary["wall_failures"].append(
                {
                    "request_id": row.get("request_id"),
                    "protocol": row.get("protocol"),
                    "case_type": row.get("case_type"),
                    "model": row.get("model"),
                    "input_wall_ok": row.get("input_wall_ok"),
                    "output_wall_ok": row.get("output_wall_ok"),
                    "finish_reason": row.get("finish_reason"),
                    "output_est_tokens": row.get("output_est_tokens"),
                }
            )
    summary["provider_header_requirement_ok"] = (
        not require_provider_header or summary["provider_header_signal_rows"] > 0
    )
    body_usage_protocols = {
        str(row.get("protocol")) for row in rows if row.get("provider_body_usage_signal")
    }
    summary["provider_body_usage_protocols"] = sorted(body_usage_protocols)
    summary["provider_body_usage_requirement_ok"] = body_usage_protocols >= {"openai", "anthropic"}
    summary["protocol_requirement_ok"] = set(summary["by_protocol"]) >= {"openai", "anthropic"}
    summary["cache_requirement_ok"] = any(row.get("cache_attempted") for row in rows) and set(
        summary["by_cache_observation"]
    ).issubset({"attempted", "accepted", "rejected", "ignored"})
    summary["observability"] = observability_summary(rows)
    summary["failure_samples"] = [
        {
            "request_id": row.get("request_id"),
            "protocol": row.get("protocol"),
            "case_type": row.get("case_type"),
            "model": row.get("model"),
            "status": row.get("status"),
            "policy_ok": row.get("policy_ok"),
            "error_class": row.get("error_class"),
            "cache_observation": row.get("cache_observation"),
            "provider_body_usage_signal": row.get("provider_body_usage_signal"),
            "provider_header_signal": row.get("provider_header_signal"),
            "result_prefix": row.get("result_prefix"),
            "stderr_prefix": row.get("stderr_prefix"),
        }
        for row in rows
        if not row.get("policy_ok")
    ][:20]
    return summary


def run_policy_matrix(
    args: argparse.Namespace,
    base_url: str,
    key: str,
    models: list[str],
    run_dir: Path,
) -> int:
    plan = build_policy_plan(args.mode, models)
    rows: list[dict[str, Any]] = []
    result_path = run_dir / "raw-results.jsonl"
    print(
        json.dumps({"event": "policy_start", "mode": args.mode, "cases": len(plan)}, ensure_ascii=False),
        flush=True,
    )
    for idx, case in enumerate(plan):
        row = run_policy_case(
            case,
            idx,
            base_url,
            key,
            args.timeout_ms,
            run_dir,
        )
        append_jsonl(result_path, row)
        rows.append(row)
        print(
            json.dumps(
                {
                    "event": "policy_result",
                    "idx": idx,
                    "protocol": row.get("protocol"),
                    "case_type": row.get("case_type"),
                    "model": row.get("model"),
                    "policy_ok": row.get("policy_ok"),
                    "error_class": row.get("error_class"),
                    "cache_observation": row.get("cache_observation"),
                    "provider_body_usage_signal": row.get("provider_body_usage_signal"),
                    "provider_header_signal": row.get("provider_header_signal"),
                    "total_ms": row.get("total_ms"),
                },
                ensure_ascii=False,
            ),
            flush=True,
        )
    summary = summarize_policy(rows, args.require_provider_header)
    safe_write_json(run_dir / "summary.json", summary)
    print(json.dumps({"event": "policy_done", "run_dir": str(run_dir), "summary": summary}, ensure_ascii=False), flush=True)
    if not summary["redaction_ok"]:
        return 3
    if not summary["provider_header_requirement_ok"]:
        return 1
    if not summary["provider_body_usage_requirement_ok"]:
        return 1
    return 0 if summary["policy_ok"] == summary["total"] else 1


def parse_cache_pressure_buckets(value: str) -> list[tuple[str, int]]:
    result: list[tuple[str, int]] = []
    for raw in value.split(","):
        item = raw.strip().lower()
        if not item:
            continue
        if item in CACHE_PRESSURE_BUCKET_TARGETS:
            result.append((item, CACHE_PRESSURE_BUCKET_TARGETS[item]))
            continue
        match = re.fullmatch(r"(\d+)(k)?", item)
        if not match:
            raise SystemExit(f"Invalid cache pressure bucket {raw!r}")
        number = int(match.group(1))
        target = number * 1000 if match.group(2) else number
        result.append((f"{number}k" if match.group(2) else str(number), target))
    if not result:
        raise SystemExit("At least one cache pressure bucket is required")
    return result


def dataset_schema() -> dict[str, Any]:
    return {
        "version": "cache-pressure-dataset-v1",
        "identity_fields": [
            "run_id",
            "request_id",
            "timestamp",
            "model",
            "client",
            "source_client",
            "protocol",
            "stream",
            "case_type",
            "prompt_bucket",
            "target_tokens",
        ],
        "shape_fields": [
            "request_body_bytes",
            "prompt_est_tokens",
            "prompt_hash",
            "prefix_4k_hash",
            "prefix_32k_hash",
            "prefix_128k_hash",
            "prefix_256k_hash",
            "cache_material_bytes",
            "request_shape_hash",
            "request_shape_tool_count",
            "request_shape_tool_name_classes",
        ],
        "latency_fields": list(LATENCY_FIELDS),
        "cache_fields": [
            "cache_observation",
            "cache_read_input_tokens",
            "cache_creation_input_tokens",
            "cache_miss_input_tokens",
            "cache_token_read_pct",
            "usage_input_tokens",
            "usage_output_tokens",
            "usage_cached_tokens",
            "usage_cache_read_tokens",
            "usage_cache_creation_tokens",
            "usage_cache_miss_tokens",
        ],
        "quality_fields": [
            "status",
            "api_ok",
            "semantic_ok",
            "tool_call_count",
            "tool_success",
            "subagent_requested",
            "subagent_observed",
            "finish_reason",
            "error_class",
        ],
        "retry_and_guard_fields": [
            "retry_count",
            "attempts_used",
            "used_disabled_thinking_retry",
            "provider_missing_reasoning_content",
            "reasoning_only_length",
            "stream_truncated",
            "client_gone",
        ],
        "privacy_policy": [
            "no raw prompt",
            "no full response",
            "no API key",
            "hash prefix only",
            "result_prefix/stderr_prefix are bounded and redacted by caller review",
        ],
    }


def build_cache_pressure_manifest(args: argparse.Namespace) -> dict[str, Any]:
    models = [item.strip() for item in args.models.split(",") if item.strip()]
    if not models:
        models = list(CACHE_PRESSURE_DEFAULT_MODELS)
    buckets = parse_cache_pressure_buckets(args.cache_pressure_buckets)
    requests_per_scenario = args.cache_pressure_rpm * args.cache_pressure_duration_minutes
    scenarios: list[dict[str, Any]] = []
    order = 0
    for model in models:
        for bucket_name, target_tokens in buckets:
            scenarios.append(
                {
                    "order": order,
                    "model": model,
                    "bucket": bucket_name,
                    "target_tokens": target_tokens,
                    "rpm": args.cache_pressure_rpm,
                    "duration_minutes": args.cache_pressure_duration_minutes,
                    "planned_requests": requests_per_scenario,
                    "warmup_seconds": args.cache_pressure_warmup_seconds,
                    "measured_window_seconds": max(
                        0,
                        args.cache_pressure_duration_minutes * 60
                        - args.cache_pressure_warmup_seconds,
                    ),
                    "client": "claudecode",
                    "protocol_mix": {
                        "text": 0.34,
                        "json": 0.16,
                        "stream_json": 0.20,
                        "bash_tool": 0.10,
                        "webfetch_tool": 0.10,
                        "websearch_tool": 0.10,
                    },
                    "stop_conditions": [
                        "zenproxy_dead_gt_0",
                        "dispatch_lt_90",
                        "newapi_or_zenproxy_5xx_gt_2pct",
                        "lane_saturated_or_no_proxy_resources",
                        "p95_first_content_or_tool_gt_30000ms_for_two_windows",
                        "tool_quality_regression",
                    ],
                }
            )
            order += 1
    return {
        "run_mode": "plan_only",
        "created_at": time.time(),
        "base_url_kind": base_url_kind(args.base_url),
        "models": models,
        "buckets": [{"name": name, "target_tokens": target} for name, target in buckets],
        "total_planned_requests": sum(item["planned_requests"] for item in scenarios),
        "execution_policy": {
            "sequence": "model_then_bucket",
            "recommended_start_rpm": min(args.cache_pressure_rpm, 20),
            "requested_rpm": args.cache_pressure_rpm,
            "production_requires_explicit_confirmation": True,
            "do_not_run_all_scenarios_in_parallel": True,
            "exclude_warmup_from_primary_metrics": True,
        },
        "scenarios": scenarios,
    }


def run_cache_pressure_plan(args: argparse.Namespace) -> int:
    run_dir = (
        Path(args.run_dir)
        if args.run_dir
        else ROOT / ".codex_tmp" / "panda-pressure" / time.strftime("%Y%m%d-%H%M%S-cache-plan")
    ).resolve()
    run_dir.mkdir(parents=True, exist_ok=True)
    manifest = build_cache_pressure_manifest(args)
    safe_write_json(run_dir / "cache-pressure-manifest.json", manifest)
    safe_write_json(run_dir / "dataset-schema.json", dataset_schema())
    safe_write_json(
        run_dir / "analysis-plan.json",
        {
            "version": "cache-pressure-analysis-v1",
            "primary_grouping": ["model", "prompt_bucket", "stream", "cache_observation"],
            "required_percentiles": [f"p{pct}" for pct in SUMMARY_PERCENTILES],
            "latency_fields": list(LATENCY_FIELDS),
            "primary_cache_metric": "token_weighted_cache_token_read_pct",
            "quality_gates": [
                "semantic_ok must not regress",
                "tool_success must not regress",
                "WebFetch/WebSearch/Bash remain enabled",
                "no context trimming or output cap",
                "no fake cache usage",
            ],
            "comparison_windows": ["all_samples", "post_warmup_only"],
        },
    )
    print(
        json.dumps(
            {
                "event": "cache_pressure_plan",
                "run_dir": str(run_dir),
                "total_planned_requests": manifest["total_planned_requests"],
            },
            ensure_ascii=False,
        ),
        flush=True,
    )
    return 0


def run_matrix(args: argparse.Namespace) -> int:
    key_name, key = env_key()
    base_url = normalize_base_url(args.base_url)
    require_panda_base(base_url, args.allow_local_panda_base)
    models = [item.strip() for item in args.models.split(",") if item.strip()]
    clients = [item.strip() for item in args.clients.split(",") if item.strip()]
    run_dir = (
        Path(args.run_dir)
        if args.run_dir
        else ROOT / ".codex_tmp" / "panda-pressure" / time.strftime("%Y%m%d-%H%M%S")
    ).resolve()
    run_dir.mkdir(parents=True, exist_ok=True)
    workspace = prepare_workspace(run_dir)
    meta = {
        "run_id": run_dir.name,
        "mode": args.mode,
        "base_url_kind": base_url_kind(base_url),
        "base_url": base_url,
        "key_env": key_name,
        "key_redacted": "sk-***" if key.startswith("sk-") else "***",
        "models": models,
        "clients": clients,
        "concurrency": args.concurrency,
        "timeout_ms": args.timeout_ms,
        "workspace": str(workspace),
        "created_at": time.time(),
        "redaction_policy": "no raw prompts, no full responses, no keys",
    }
    safe_write_json(run_dir / "meta.json", meta)
    print(json.dumps({"event": "start", **meta}, ensure_ascii=False), flush=True)
    pf = preflight(base_url, key, models, args.preflight_timeout_s)
    safe_write_json(run_dir / "preflight.json", pf)
    print(json.dumps({"event": "preflight", **pf}, ensure_ascii=False), flush=True)
    if args.mode == "preflight":
        return 0 if pf.get("ok") else 2
    if not pf.get("ok") and not args.force:
        print(
            json.dumps(
                {
                    "event": "blocked",
                    "reason": "preflight failed; pass --force to continue",
                    "error_class": pf.get("error_class"),
                    "blocker": pf.get("blocker"),
                },
                ensure_ascii=False,
            ),
            flush=True,
        )
        return 2
    if args.mode in POLICY_MODES:
        return run_policy_matrix(args, base_url, key, models, run_dir)
    plan = build_plan(args.mode, args.rounds_per_client)
    rows: list[dict[str, Any]] = []
    result_path = run_dir / "raw-results.jsonl"
    for client in clients:
        client_rows: list[dict[str, Any]] = []
        print(json.dumps({"event": "client_start", "client": client, "cases": len(plan)}, ensure_ascii=False), flush=True)
        # Keep full client tools sequential by default. A small thread pool is still
        # useful for HTTP-like short CLI cases when the caller opts in.
        if args.concurrency <= 1:
            for idx, case in enumerate(plan):
                row = run_case(
                    client,
                    case,
                    idx,
                    model_for_index(models, idx),
                    workspace,
                    base_url,
                    key,
                    args.timeout_ms,
                    run_dir,
                    models,
                )
                append_jsonl(result_path, row)
                rows.append(row)
                client_rows.append(row)
                print(
                    json.dumps(
                        {
                            "event": "result",
                            "client": client,
                            "idx": idx,
                            "case_type": case.case_type,
                            "model": row.get("model"),
                            "status": row.get("status"),
                            "semantic_ok": row.get("semantic_ok"),
                            "error_class": row.get("error_class"),
                            "total_ms": row.get("total_ms"),
                        },
                        ensure_ascii=False,
                    ),
                    flush=True,
                )
        else:
            from concurrent.futures import ThreadPoolExecutor, as_completed

            with ThreadPoolExecutor(max_workers=args.concurrency) as executor:
                futures = {
                    executor.submit(
                        run_case,
                        client,
                        case,
                        idx,
                        model_for_index(models, idx),
                        workspace,
                        base_url,
                        key,
                        args.timeout_ms,
                        run_dir,
                        models,
                    ): (idx, case)
                    for idx, case in enumerate(plan)
                }
                for fut in as_completed(futures):
                    idx, case = futures[fut]
                    row = fut.result()
                    append_jsonl(result_path, row)
                    rows.append(row)
                    client_rows.append(row)
                    print(
                        json.dumps(
                            {
                                "event": "result",
                                "client": client,
                                "idx": idx,
                                "case_type": case.case_type,
                                "model": row.get("model"),
                                "status": row.get("status"),
                                "semantic_ok": row.get("semantic_ok"),
                                "error_class": row.get("error_class"),
                                "total_ms": row.get("total_ms"),
                            },
                            ensure_ascii=False,
                        ),
                        flush=True,
                    )
        client_summary = summarize(client_rows)
        safe_write_json(run_dir / f"summary-{client}.json", client_summary)
        print(
            json.dumps({"event": "client_done", "client": client, "summary": client_summary}, ensure_ascii=False),
            flush=True,
        )
    summary = summarize(rows)
    safe_write_json(run_dir / "summary.json", summary)
    print(json.dumps({"event": "done", "run_dir": str(run_dir), "summary": summary}, ensure_ascii=False), flush=True)
    if not summary["redaction_ok"]:
        return 3
    if args.mode == "smoke":
        return 0 if summary["ok"] == summary["total"] else 1
    return 0 if summary["ok"] == summary["total"] and summary["semantic_ok"] == summary["total"] else 1


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--mode",
        choices=[
            "preflight",
            "smoke",
            "dry",
            "full",
            "policy-smoke",
            "policy-dry",
            "cache-pressure-plan",
        ],
        default="smoke",
    )
    parser.add_argument(
        "--clients",
        default="windows-claudecode,wsl-claudecode,wsl-hermes,wsl-openclaw",
        help="Comma-separated clients: windows-claudecode,wsl-claudecode,wsl-hermes,wsl-openclaw",
    )
    parser.add_argument("--models", default=",".join(DEFAULT_MODELS))
    parser.add_argument("--base-url", default=os.environ.get("PANDA_NEWAPI_BASE_URL", DEFAULT_BASE_URL))
    parser.add_argument("--allow-local-panda-base", action="store_true")
    parser.add_argument("--run-dir", default="")
    parser.add_argument("--rounds-per-client", type=int, default=None)
    parser.add_argument("--concurrency", type=int, default=1)
    parser.add_argument("--timeout-ms", type=int, default=300000)
    parser.add_argument("--preflight-timeout-s", type=int, default=30)
    parser.add_argument("--force", action="store_true")
    parser.add_argument(
        "--require-provider-header",
        action="store_true",
        help="Fail policy modes unless an allowlisted provider header is observed.",
    )
    parser.add_argument(
        "--cache-pressure-buckets",
        default="10k,50k,100k,200k",
        help="Comma-separated target context buckets for cache-pressure-plan.",
    )
    parser.add_argument("--cache-pressure-rpm", type=int, default=20)
    parser.add_argument("--cache-pressure-duration-minutes", type=int, default=5)
    parser.add_argument("--cache-pressure-warmup-seconds", type=int, default=60)
    return parser.parse_args(argv)


def main(argv: list[str]) -> int:
    args = parse_args(argv)
    if args.mode in PLAN_ONLY_MODES:
        return run_cache_pressure_plan(args)
    return run_matrix(args)


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
