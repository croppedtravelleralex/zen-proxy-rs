#!/usr/bin/env python3
"""
Create a redacted ZenProxy/NewAPI test evidence package.

This script is intentionally read-only against services. It collects ZenProxy
admin/audit snapshots, optional NewAPI exported logs, and writes a stable run
directory that both humans and agents can inspect.
"""

from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import os
import re
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path
from typing import Any


SECRET_PATTERNS = [
    re.compile(r"sk-[A-Za-z0-9_\-]{6,}"),
    re.compile(r"Bearer\s+[A-Za-z0-9._\-]+", re.IGNORECASE),
    re.compile(r"(api[_-]?key|authorization|token)([\"'\s:=]+)([^\"'\s,}]+)", re.IGNORECASE),
    re.compile(r"(https?://)([^/@:\s]+):([^/@\s]+)@"),
]


def now_ms() -> int:
    return int(time.time() * 1000)


def redacted_text(text: str) -> str:
    out = text
    for pat in SECRET_PATTERNS:
        if pat.pattern.startswith("(https?://)"):
            out = pat.sub(r"\1[REDACTED]@", out)
        elif "api" in pat.pattern.lower() or "authorization" in pat.pattern.lower():
            out = pat.sub(r"\1\2[REDACTED]", out)
        else:
            out = pat.sub("[REDACTED]", out)
    return out


def redact_obj(value: Any) -> Any:
    if isinstance(value, dict):
        cleaned: dict[str, Any] = {}
        for key, item in value.items():
            lk = str(key).lower()
            if lk in {"authorization", "x-api-key", "api_key", "apikey", "token", "cookie"}:
                cleaned[key] = "[REDACTED]"
            elif lk in {"messages", "prompt", "completion", "content", "tool_output", "request_body"}:
                cleaned[key] = "[OMITTED]"
            elif lk in {
                "selected_node_url",
                "selected_node_url_redacted",
                "node_url",
                "node_url_redacted",
                "proxy_url",
            }:
                cleaned[key] = redact_url(str(item))
            else:
                cleaned[key] = redact_obj(item)
        return cleaned
    if isinstance(value, list):
        return [redact_obj(item) for item in value]
    if isinstance(value, str):
        return redacted_text(value)
    return value


def redact_url(url: str) -> str:
    try:
        parsed = urllib.parse.urlsplit(url)
        if parsed.username or parsed.password:
            netloc = parsed.hostname or "unknown"
            if parsed.port:
                netloc = f"{netloc}:{parsed.port}"
            return urllib.parse.urlunsplit((parsed.scheme, netloc, parsed.path, "", ""))
    except Exception:
        pass
    return redacted_text(url)


def fetch(url: str, admin_key: str | None, timeout: float) -> tuple[int, str]:
    headers = {"Accept": "application/json,text/plain,*/*"}
    if admin_key:
        headers["Authorization"] = f"Bearer {admin_key}"
    req = urllib.request.Request(url, headers=headers)
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            body = resp.read().decode("utf-8", errors="replace")
            return resp.getcode(), body
    except urllib.error.HTTPError as exc:
        body = exc.read().decode("utf-8", errors="replace")
        return exc.code, body
    except Exception as exc:
        return 0, json.dumps({"error": type(exc).__name__, "message": str(exc)})


def write_json(path: Path, payload: Any) -> None:
    path.write_text(json.dumps(payload, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")


def write_raw_response(path: Path, status: int, body: str) -> Any:
    redacted = redacted_text(body)
    try:
        parsed = json.loads(redacted)
        parsed = redact_obj(parsed)
        write_json(path, {"status": status, "body": parsed})
        return parsed
    except Exception:
        path.write_text(json.dumps({"status": status, "body": redacted}, ensure_ascii=False) + "\n", encoding="utf-8")
        return {"status": status, "body": redacted}


def extract_records(export_body: Any) -> list[dict[str, Any]]:
    if isinstance(export_body, dict):
        body = export_body.get("body")
    else:
        body = export_body
    if isinstance(body, dict):
        if "body" in body:
            return extract_records({"body": body.get("body")})
        for key in ("data", "requests", "records"):
            if isinstance(body.get(key), list):
                return [item for item in body[key] if isinstance(item, dict)]
            if isinstance(body.get(key), dict):
                nested = body[key]
                for nested_key in ("data", "requests", "records"):
                    if isinstance(nested.get(nested_key), list):
                        return [item for item in nested[nested_key] if isinstance(item, dict)]
                if nested.get("rid") or nested.get("request_id"):
                    return [nested]
        if body.get("rid") or body.get("request_id"):
            return [body]
    if isinstance(body, str):
        stripped = body.strip()
        if stripped:
            try:
                parsed = json.loads(stripped)
            except Exception:
                parsed = None
            if isinstance(parsed, list):
                return [item for item in parsed if isinstance(item, dict)]
            if isinstance(parsed, dict):
                return extract_records({"body": parsed})
        records = []
        for line in body.splitlines():
            line = line.strip()
            if not line:
                continue
            try:
                item = json.loads(line)
            except Exception:
                continue
            if isinstance(item, dict):
                records.append(item)
        return records
    return []


def short_hash(value: str) -> str:
    return hashlib.sha256(value.encode("utf-8", errors="ignore")).hexdigest()[:12]


def parse_time_ms(value: Any) -> int:
    if value in (None, ""):
        return 0
    if isinstance(value, (int, float)):
        number = float(value)
        if number <= 0:
            return 0
        return int(number * 1000) if number < 10_000_000_000 else int(number)
    text = str(value).strip()
    if not text:
        return 0
    try:
        number = float(text)
        return int(number * 1000) if number < 10_000_000_000 else int(number)
    except Exception:
        pass
    try:
        normalized = text.replace("Z", "+00:00")
        parsed = dt.datetime.fromisoformat(normalized)
        if parsed.tzinfo is None:
            parsed = parsed.replace(tzinfo=dt.timezone.utc)
        return int(parsed.timestamp() * 1000)
    except Exception:
        return 0


def as_text(value: Any) -> str:
    if value is None:
        return ""
    return str(value)


def metric_from_record(run_id: str, record: dict[str, Any]) -> dict[str, Any]:
    timings = record.get("timings") or {}
    context = record.get("context") or record.get("context_governance") or {}
    usage = record.get("usage") or {}
    guard = record.get("protocol_guard") or {}
    return {
        "run_id": run_id,
        "ts_ms": parse_time_ms(record.get("timestamp_ms") or record.get("ts_ms") or record.get("ts") or record.get("created_at")),
        "rid": record.get("rid") or record.get("request_id") or "",
        "external_request_id": record.get("external_request_id") or "",
        "gateway": record.get("gateway") or "",
        "gateway_channel_id": record.get("gateway_channel_id") or "",
        "model": record.get("public_model") or record.get("model") or "",
        "upstream_model": record.get("upstream_model") or "",
        "protocol": record.get("protocol") or record.get("path") or "",
        "stream": bool(record.get("stream") or record.get("is_streaming")),
        "status": record.get("status") or record.get("status_code") or 0,
        "outcome": record.get("outcome") or "",
        "failure_kind": record.get("failure_kind") or "",
        "failure_message_class": short_hash(str(record.get("failure_message") or "")) if record.get("failure_message") else "",
        "pool": {
            "selected_node_id": record.get("selected_node_id") or "",
            "selected_node_url_redacted": redact_url(str(record.get("selected_node_url_redacted") or record.get("selected_node_url") or "")),
            "retry_count": record.get("retry_count") or 0,
            "retry_chain": redact_obj(record.get("retry_chain") or []),
        },
        "timings": {
            "upstream_response_ms": timings.get("upstream_response_ms") or record.get("upstream_ms") or 0,
            "first_chunk_ms": timings.get("first_chunk_ms") or record.get("ttft_ms") or 0,
            "first_content_token_ms": timings.get("first_content_token_ms") or 0,
            "first_tool_call_ms": timings.get("first_tool_call_ms") or 0,
            "stream_complete_ms": timings.get("stream_complete_ms") or 0,
            "total_ms": timings.get("total_ms") or record.get("latency_total_ms") or 0,
        },
        "context": {
            "body_size_bucket": record.get("body_size_bucket") or context.get("body_size_bucket") or "",
            "original_body_bytes": context.get("original_body_bytes") or 0,
            "effective_body_bytes": context.get("effective_body_bytes") or 0,
            "trimmed": bool(context.get("trimmed") or context.get("action") == "compact"),
            "trimmed_bytes": context.get("trimmed_bytes") or 0,
        },
        "usage": {
            "prompt_tokens": usage.get("prompt_tokens") or record.get("prompt_tokens") or 0,
            "completion_tokens": usage.get("completion_tokens") or record.get("completion_tokens") or 0,
            "total_tokens": usage.get("total_tokens") or 0,
            "cached_tokens": usage.get("cached_tokens") or 0,
            "cache_read_input_tokens": usage.get("cache_read_input_tokens") or 0,
            "cache_creation_input_tokens": usage.get("cache_creation_input_tokens") or 0,
        },
        "protocol_guard": redact_obj(guard),
        "privacy": {
            "raw_body_stored": False,
            "redaction_version": "v1",
            "contains_secret": False,
        },
    }


def load_json_or_jsonl(path: Path) -> list[dict[str, Any]]:
    text = path.read_text(encoding="utf-8", errors="replace")
    stripped = text.strip()
    if not stripped:
        return []
    try:
        parsed = json.loads(stripped)
    except Exception:
        parsed = None
    if isinstance(parsed, list):
        return [redact_obj(item) for item in parsed if isinstance(item, dict)]
    if isinstance(parsed, dict):
        records = extract_records({"body": parsed})
        return [redact_obj(item) for item in records]

    records: list[dict[str, Any]] = []
    for line in text.splitlines():
        line = line.strip()
        if not line:
            continue
        try:
            item = json.loads(line)
        except Exception:
            continue
        if isinstance(item, dict):
            records.append(redact_obj(item))
    return records


def normalize_newapi_record(record: dict[str, Any]) -> dict[str, Any]:
    request_id = as_text(
        record.get("request_id")
        or record.get("external_request_id")
        or record.get("id")
        or record.get("requestId")
        or ""
    )
    status = record.get("status")
    if status is None:
        status = record.get("status_code") or record.get("code") or ""
    if status == "" and record.get("type") in (2, "2"):
        status = 200
    duration_ms = record.get("duration_ms")
    if duration_ms is None:
        duration_ms = record.get("elapsed_ms") or record.get("latency_ms") or 0
    if not duration_ms and record.get("use_time") not in (None, ""):
        duration_ms = int(float(record.get("use_time") or 0) * 1000)
    elif not duration_ms and record.get("duration") not in (None, ""):
        raw_duration = float(record.get("duration") or 0)
        duration_ms = int(raw_duration * 1000) if 0 < raw_duration < 1000 else raw_duration
    created_at = record.get("created_at") or record.get("createdAt") or record.get("created_time")
    return {
        "request_id": request_id,
        "channel_id": as_text(record.get("channel_id") or record.get("channelId") or ""),
        "upstream_request_id": as_text(record.get("upstream_request_id") or record.get("upstreamRequestId") or ""),
        "model": as_text(record.get("model") or record.get("model_name") or ""),
        "status": status,
        "duration_ms": duration_ms,
        "created_at": as_text(created_at or ""),
        "created_at_ms": parse_time_ms(created_at),
        "stream": bool(record.get("stream") or record.get("is_stream")),
        "prompt_tokens": int(record.get("prompt_tokens") or 0),
        "completion_tokens": int(record.get("completion_tokens") or 0),
    }


def normalize_status(value: Any) -> str:
    text = as_text(value).strip().lower()
    if not text:
        return ""
    try:
        return str(int(float(text)))
    except Exception:
        return text


def make_request_map_row(
    run_id: str,
    join_status: str,
    join_key: str,
    newapi: dict[str, Any] | None = None,
    zen: dict[str, Any] | None = None,
    join_delta_ms: int | None = None,
) -> dict[str, Any]:
    newapi = newapi or {}
    zen = zen or {}
    return {
        "run_id": run_id,
        "client_request_id": "",
        "newapi_request_id": as_text(newapi.get("request_id")),
        "newapi_upstream_request_id": as_text(newapi.get("upstream_request_id")),
        "newapi_channel_id": as_text(newapi.get("channel_id")),
        "newapi_model": as_text(newapi.get("model")),
        "newapi_status": newapi.get("status") if newapi.get("status") is not None else "",
        "newapi_duration_ms": newapi.get("duration_ms") if newapi.get("duration_ms") is not None else 0,
        "newapi_created_at": as_text(newapi.get("created_at")),
        "newapi_created_at_ms": int(newapi.get("created_at_ms") or 0),
        "newapi_stream": bool(newapi.get("stream")),
        "newapi_prompt_tokens": int(newapi.get("prompt_tokens") or 0),
        "newapi_completion_tokens": int(newapi.get("completion_tokens") or 0),
        "zen_rid": as_text(zen.get("rid")),
        "zen_external_request_id": as_text(zen.get("external_request_id")),
        "zen_gateway": as_text(zen.get("gateway")),
        "zen_gateway_channel_id": as_text(zen.get("gateway_channel_id")),
        "zen_model": as_text(zen.get("model")),
        "zen_status": zen.get("status") if zen.get("status") is not None else "",
        "zen_ts_ms": int(zen.get("ts_ms") or 0),
        "join_status": join_status,
        "join_key": join_key,
        "join_delta_ms": join_delta_ms if join_delta_ms is not None else 0,
    }


def build_request_map(run_id: str, metrics: list[dict[str, Any]], newapi_records: list[dict[str, Any]], time_window_ms: int) -> list[dict[str, Any]]:
    rows: list[dict[str, Any]] = []
    matched_newapi: set[int] = set()
    matched_zen: set[int] = set()

    zen_by_external: dict[str, list[int]] = {}
    for idx, item in enumerate(metrics):
        key = as_text(item.get("external_request_id")).strip()
        if key:
            zen_by_external.setdefault(key, []).append(idx)

    newapi_by_external: dict[str, list[int]] = {}
    for idx, item in enumerate(newapi_records):
        key = as_text(item.get("request_id")).strip()
        if key:
            newapi_by_external.setdefault(key, []).append(idx)

    for key in sorted(set(zen_by_external) & set(newapi_by_external)):
        zen_idxs = zen_by_external.get(key, [])
        newapi_idxs = newapi_by_external.get(key, [])
        if len(zen_idxs) == 1 and len(newapi_idxs) == 1:
            zi = zen_idxs[0]
            ni = newapi_idxs[0]
            rows.append(make_request_map_row(run_id, "matched", "external_request_id", newapi_records[ni], metrics[zi]))
            matched_zen.add(zi)
            matched_newapi.add(ni)
        else:
            for ni in newapi_idxs:
                rows.append(make_request_map_row(run_id, "ambiguous", "external_request_id", newapi_records[ni], None))
                matched_newapi.add(ni)
            for zi in zen_idxs:
                rows.append(make_request_map_row(run_id, "ambiguous", "external_request_id", None, metrics[zi]))
                matched_zen.add(zi)

    candidates_by_pair: list[tuple[int, int, int]] = []
    for ni, newapi in enumerate(newapi_records):
        if ni in matched_newapi:
            continue
        created_at_ms = int(newapi.get("created_at_ms") or 0)
        if not created_at_ms:
            continue
        for zi, zen in enumerate(metrics):
            if zi in matched_zen:
                continue
            if as_text(newapi.get("model")) and as_text(newapi.get("model")) != as_text(zen.get("model")):
                continue
            if normalize_status(newapi.get("status")) and normalize_status(newapi.get("status")) != normalize_status(zen.get("status")):
                continue
            zen_ts_ms = int(zen.get("ts_ms") or 0)
            delta = abs(zen_ts_ms - created_at_ms) if zen_ts_ms else 0
            if delta and delta <= time_window_ms:
                candidates_by_pair.append((delta, ni, zi))

    newapi_time_candidates = {ni for _, ni, _ in candidates_by_pair}
    zen_time_candidates = {zi for _, _, zi in candidates_by_pair}
    for delta, ni, zi in sorted(candidates_by_pair):
        if ni in matched_newapi or zi in matched_zen:
            continue
        rows.append(make_request_map_row(run_id, "matched", "time_model_status_nearest", newapi_records[ni], metrics[zi], delta))
        matched_newapi.add(ni)
        matched_zen.add(zi)

    for ni in sorted(newapi_time_candidates - matched_newapi):
        rows.append(make_request_map_row(run_id, "ambiguous", "time_model_status_nearest", newapi_records[ni], None))
        matched_newapi.add(ni)
    for zi in sorted(zen_time_candidates - matched_zen):
        rows.append(make_request_map_row(run_id, "ambiguous", "time_model_status_nearest", None, metrics[zi]))
        matched_zen.add(zi)

    for ni, item in enumerate(newapi_records):
        if ni not in matched_newapi:
            rows.append(make_request_map_row(run_id, "newapi_only", "unmatched", item, None))
    for zi, item in enumerate(metrics):
        if zi not in matched_zen:
            rows.append(make_request_map_row(run_id, "zen_only", "unmatched", None, item))
    return rows


def collect_raw_records_from_run(raw_dir: Path) -> tuple[list[dict[str, Any]], list[str]]:
    records: list[dict[str, Any]] = []
    raw_files: list[str] = []
    for path in sorted(raw_dir.glob("*zen-audit-*.redacted.json*")):
        if "summary" in path.name or "anomalies" in path.name:
            continue
        parsed = load_json_or_jsonl(path)
        if parsed:
            records.extend(parsed)
            raw_files.append(f"raw/{path.name}")
    return records, raw_files


def summarize(
    run_id: str,
    metrics: list[dict[str, Any]],
    raw_files: list[str],
    source_statuses: list[dict[str, Any]],
    request_map_rows: list[dict[str, Any]] | None = None,
) -> str:
    total = len(metrics)
    ok = sum(1 for item in metrics if int(item.get("status") or 0) < 400 and not item.get("failure_kind"))
    request_map_rows = request_map_rows or []
    request_map_statuses: dict[str, int] = {}
    for row in request_map_rows:
        status = str(row.get("join_status") or "unknown")
        request_map_statuses[status] = request_map_statuses.get(status, 0) + 1
    failures: dict[str, int] = {}
    slow_ttft = 0
    guard_repairs = 0
    for item in metrics:
        fk = item.get("failure_kind") or "none"
        failures[fk] = failures.get(fk, 0) + 1
        timings = item.get("timings") or {}
        if int(timings.get("first_content_token_ms") or timings.get("first_chunk_ms") or 0) >= 10_000:
            slow_ttft += 1
        guard = item.get("protocol_guard") or {}
        if guard.get("applied") or guard.get("pre_invalid") or guard.get("synthetic_tool_id_count"):
            guard_repairs += 1
    lines = [
        f"# Test Run {run_id}",
        "",
        "## 结论",
        f"采集完成。ZenProxy audit 记录 {total} 条，成功估算 {ok} 条，失败/异常 {total - ok} 条。",
        "",
        "## 请求链路",
        "ZenProxy 侧记录来自 admin/audit；NewAPI 映射来自 --newapi-log-path（如提供）。",
        "",
        "## 采集源健康",
    ]
    by_source: dict[str, list[int]] = {}
    for item in source_statuses:
        by_source.setdefault(str(item["source"]), []).append(int(item["status"]))
    for source, statuses in sorted(by_source.items()):
        ok_count = sum(1 for status in statuses if 200 <= status < 300)
        lines.append(f"- {source}: {ok_count}/{len(statuses)} endpoints ok")
    lines.extend([
        "",
        "## 关键指标",
        f"- requests: {total}",
        f"- success_estimated: {ok}",
        f"- slow_ttft_ge_10s: {slow_ttft}",
        f"- request_map_records: {len(request_map_rows)}",
        f"- request_map_matched: {request_map_statuses.get('matched', 0)}",
        "",
        "## 工具调用修复",
        f"- protocol_guard_related_records: {guard_repairs}",
        "",
        "## 异常",
    ])
    for key, count in sorted(failures.items(), key=lambda kv: (-kv[1], kv[0]))[:10]:
        lines.append(f"- {key}: {count}")
    lines.extend(["", "## 原始材料"])
    for file_name in raw_files:
        lines.append(f"- {file_name}")
    lines.append("")
    return "\n".join(lines)


def main() -> int:
    parser = argparse.ArgumentParser(description="Collect a redacted ZenProxy test evidence package.")
    parser.add_argument("--scenario", default="manual", help="scenario name used in run directory")
    parser.add_argument("--run-id", default="", help="optional run id")
    parser.add_argument("--out-dir", default="test-records/runs", help="output runs directory")
    parser.add_argument("--zen-base-url", default=os.getenv("ZEN_BASE_URL", "http://127.0.0.1:4000"))
    parser.add_argument(
        "--zen-admin-base-url",
        action="append",
        default=[],
        help="admin base URL to collect; repeat for multi-instance deployments",
    )
    parser.add_argument("--newapi-base-url", default=os.getenv("NEWAPI_BASE_URL", "http://127.0.0.1:8081"))
    parser.add_argument("--newapi-log-path", default="", help="optional NewAPI JSON array or JSONL log export")
    parser.add_argument("--raw-run-path", default="", help="rebuild derived outputs from an existing run directory")
    parser.add_argument("--join-time-window-ms", type=int, default=5000)
    parser.add_argument("--admin-key", default=os.getenv("ZEN_ADMIN_KEY", "test-key"))
    parser.add_argument("--from-ms", type=int, default=0)
    parser.add_argument("--to-ms", type=int, default=0)
    parser.add_argument("--timeout", type=float, default=8.0)
    args = parser.parse_args()

    started = now_ms()
    from_ms = args.from_ms or started - 15 * 60 * 1000
    to_ms = args.to_ms or now_ms()
    run_id = args.run_id or time.strftime("%Y%m%d-%H%M%S") + f"-{args.scenario}"
    run_dir = Path(args.out_dir) / run_id
    if args.raw_run_path:
        run_dir = Path(args.raw_run_path)
        run_id = run_dir.name
    raw_dir = run_dir / "raw"
    derived_dir = run_dir / "derived"
    raw_dir.mkdir(parents=True, exist_ok=True)
    derived_dir.mkdir(parents=True, exist_ok=True)

    admin_base_urls = args.zen_admin_base_url or [args.zen_base_url]

    manifest = {
        "schema_version": "test-records.v1",
        "run_id": run_id,
        "scenario": args.scenario,
        "started_at_ms": started,
        "ended_at_ms": None,
        "environment": {
            "zenproxy_base_url": args.zen_base_url,
            "zenproxy_admin_base_urls": admin_base_urls,
            "newapi_base_url": args.newapi_base_url,
        },
        "time_window": {"from_ms": from_ms, "to_ms": to_ms},
        "expected": {
            "http_2xx_min": 1,
            "no_upstream_tool_id_error": True,
            "audit_join_rate_min": 0.95,
        },
    }

    raw_files: list[str] = []
    captured: dict[str, Any] = {}
    export_records: list[dict[str, Any]] = []
    source_statuses: list[dict[str, Any]] = []

    endpoints = {
        "zen-admin-runtime.redacted.json": "/admin/runtime",
        "zen-admin-config.redacted.json": "/admin/config",
        "zen-admin-pools.redacted.json": "/admin/pools",
        "zen-admin-budget.redacted.json": "/admin/budget",
        "zen-admin-budget-nodes.redacted.json": "/admin/budget/nodes",
        "zen-audit-summary.redacted.json": f"/admin/audit/summary?from={from_ms}&to={to_ms}",
        "zen-audit-anomalies.redacted.json": f"/admin/audit/anomalies?from={from_ms}&to={to_ms}&limit=1000",
        "zen-audit-export.redacted.jsonl": f"/admin/audit/export?from={from_ms}&to={to_ms}&format=jsonl&limit=10000",
        "zen-audit-requests.redacted.json": f"/admin/audit/requests?from={from_ms}&to={to_ms}&limit=10000",
    }

    if args.raw_run_path:
        export_records, raw_files = collect_raw_records_from_run(raw_dir)
        manifest_path = run_dir / "manifest.json"
        if manifest_path.exists():
            try:
                existing_manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
                manifest.update(existing_manifest)
                manifest["run_id"] = run_id
                manifest["recollected_at_ms"] = started
            except Exception:
                pass
    else:
        for idx, base_url in enumerate(admin_base_urls):
            prefix = f"zen{idx}"
            for file_name, path in endpoints.items():
                out_name = f"{prefix}-{file_name}"
                url = base_url.rstrip("/") + path
                status, body = fetch(url, args.admin_key, args.timeout)
                parsed = write_raw_response(raw_dir / out_name, status, body)
                source_statuses.append({"source": base_url, "endpoint": path, "status": status})
                raw_files.append(f"raw/{out_name}")
                captured[out_name] = parsed
                if file_name in {"zen-audit-export.redacted.jsonl", "zen-audit-requests.redacted.json"}:
                    export_records.extend(extract_records(parsed))

            metrics_status, metrics_body = fetch(base_url.rstrip("/") + "/metrics", None, args.timeout)
            metrics_name = f"{prefix}-zen-metrics.prom"
            (raw_dir / metrics_name).write_text(redacted_text(metrics_body), encoding="utf-8")
            raw_files.append(f"raw/{metrics_name}")
            captured[metrics_name] = {"status": metrics_status}
            source_statuses.append({"source": base_url, "endpoint": "/metrics", "status": metrics_status})

    newapi_records: list[dict[str, Any]] = []
    if args.newapi_log_path:
        newapi_path = Path(args.newapi_log_path)
        newapi_records = [normalize_newapi_record(item) for item in load_json_or_jsonl(newapi_path)]
        newapi_raw_name = "newapi-logs.redacted.jsonl"
        with (raw_dir / newapi_raw_name).open("w", encoding="utf-8") as fh:
            for item in newapi_records:
                fh.write(json.dumps(item, ensure_ascii=False) + "\n")
        raw_files.append(f"raw/{newapi_raw_name}")

    seen: set[str] = set()
    unique_export_records: list[dict[str, Any]] = []
    for record in export_records:
        rid = str(record.get("rid") or record.get("request_id") or "")
        key = rid or json.dumps(record, sort_keys=True, ensure_ascii=False)
        if key in seen:
            continue
        seen.add(key)
        unique_export_records.append(record)
    export_records = unique_export_records
    metrics = [metric_from_record(run_id, item) for item in export_records]
    with (derived_dir / "metrics.jsonl").open("w", encoding="utf-8") as fh:
        for item in metrics:
            fh.write(json.dumps(item, ensure_ascii=False) + "\n")

    request_map_rows = build_request_map(run_id, metrics, newapi_records, args.join_time_window_ms)
    with (derived_dir / "request-map.jsonl").open("w", encoding="utf-8") as fh:
        for item in request_map_rows:
            fh.write(json.dumps(item, ensure_ascii=False) + "\n")

    guard_summary = {
        "records": len(metrics),
        "applied": sum(1 for item in metrics if (item.get("protocol_guard") or {}).get("applied")),
        "post_invalid": [
            item.get("rid") for item in metrics
            if (item.get("protocol_guard") or {}).get("post_valid") is False
        ],
    }
    write_json(derived_dir / "tool-repair-summary.json", guard_summary)

    manifest["ended_at_ms"] = now_ms()
    write_json(run_dir / "manifest.json", manifest)
    (run_dir / "summary.md").write_text(
        summarize(run_id, metrics, raw_files, source_statuses, request_map_rows),
        encoding="utf-8",
    )
    print(str(run_dir))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
