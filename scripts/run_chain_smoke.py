#!/usr/bin/env python3
"""
Run a minimal Chain-of-Custody smoke test through NewAPI and collect evidence.

Environment:
  NEWAPI_BASE_URL     default http://127.0.0.1:8081
  NEWAPI_API_KEY      default sk-dev
  ZEN_BASE_URL        default http://127.0.0.1:4000
  ZEN_ADMIN_KEY       default test-key
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path
from typing import Any


def now_ms() -> int:
    return int(time.time() * 1000)


def request_json(url: str, key: str, payload: dict[str, Any] | None, timeout: float, stream: bool = False) -> dict[str, Any]:
    headers = {
        "Accept": "application/json,text/event-stream",
        "Content-Type": "application/json",
        "Authorization": f"Bearer {key}",
    }
    data = None if payload is None else json.dumps(payload).encode("utf-8")
    req = urllib.request.Request(url, data=data, headers=headers, method="GET" if payload is None else "POST")
    started = time.perf_counter()
    status = 0
    first_byte_ms = 0
    first_content_ms = 0
    body_parts: list[bytes] = []
    error = ""
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            status = resp.getcode()
            if stream:
                while True:
                    chunk = resp.readline()
                    if not chunk:
                        break
                    if first_byte_ms == 0:
                        first_byte_ms = int((time.perf_counter() - started) * 1000)
                    body_parts.append(chunk)
                    if first_content_ms == 0 and b"content" in chunk and b"delta" in chunk:
                        first_content_ms = int((time.perf_counter() - started) * 1000)
            else:
                body = resp.read()
                first_byte_ms = int((time.perf_counter() - started) * 1000)
                body_parts.append(body)
    except urllib.error.HTTPError as exc:
        status = exc.code
        body_parts.append(exc.read())
        error = f"HTTPError:{exc.code}"
    except Exception as exc:
        error = f"{type(exc).__name__}:{exc}"
    total_ms = int((time.perf_counter() - started) * 1000)
    body = b"".join(body_parts).decode("utf-8", errors="replace")
    return {
        "status": status,
        "first_byte_ms": first_byte_ms,
        "first_content_ms": first_content_ms,
        "total_ms": total_ms,
        "body_preview": body[:800],
        "body_bytes": len(body.encode("utf-8", errors="ignore")),
        "error": error,
    }


def summarize_content(content: Any) -> dict[str, Any]:
    if isinstance(content, str):
        return {"shape": "text", "text_len": len(content)}
    if isinstance(content, list):
        blocks: list[dict[str, Any]] = []
        for block in content:
            if isinstance(block, dict):
                summary: dict[str, Any] = {"type": block.get("type", "unknown")}
                if "id" in block:
                    summary["has_id"] = True
                if "tool_use_id" in block:
                    summary["has_tool_use_id"] = True
                if "name" in block:
                    summary["name"] = block.get("name")
                if "text" in block:
                    summary["text_len"] = len(str(block.get("text") or ""))
                if "content" in block:
                    summary["content_shape"] = "text" if isinstance(block.get("content"), str) else type(block.get("content")).__name__
                blocks.append(summary)
            else:
                blocks.append({"type": type(block).__name__})
        return {"shape": "blocks", "block_count": len(content), "blocks": blocks}
    if content is None:
        return {"shape": "null"}
    return {"shape": type(content).__name__}


def summarize_messages(messages: Any) -> dict[str, Any]:
    if not isinstance(messages, list):
        return {"shape": type(messages).__name__}
    return {
        "shape": "messages",
        "count": len(messages),
        "items": [
            {
                "role": msg.get("role") if isinstance(msg, dict) else None,
                "content": summarize_content(msg.get("content")) if isinstance(msg, dict) else {"shape": type(msg).__name__},
                "has_tool_calls": bool(isinstance(msg, dict) and msg.get("tool_calls")),
                "tool_call_count": len(msg.get("tool_calls") or []) if isinstance(msg, dict) else 0,
                "has_tool_call_id": bool(isinstance(msg, dict) and msg.get("tool_call_id")),
            }
            for msg in messages
        ],
    }


def summarize_response_body(body: str) -> dict[str, Any]:
    if not body:
        return {"shape": "empty"}
    if body.lstrip().startswith("data:") or "\nevent:" in body:
        events: list[str] = []
        data_lines = 0
        for line in body.splitlines():
            if line.startswith("event:"):
                events.append(line.removeprefix("event:").strip())
            elif line.startswith("data:"):
                data_lines += 1
        return {
            "shape": "sse",
            "event_count": len(events),
            "events": events[:20],
            "data_line_count": data_lines,
            "contains_done": "[DONE]" in body,
        }
    try:
        parsed = json.loads(body)
    except json.JSONDecodeError:
        return {"shape": "text", "text_len": len(body)}
    if not isinstance(parsed, dict):
        return {"shape": type(parsed).__name__}
    summary: dict[str, Any] = {
        "shape": "json_object",
        "keys": sorted(str(key) for key in parsed.keys()),
    }
    if isinstance(parsed.get("content"), list):
        summary["content_blocks"] = [
            {"type": block.get("type", "unknown")} if isinstance(block, dict) else {"type": type(block).__name__}
            for block in parsed["content"]
        ]
    if isinstance(parsed.get("choices"), list):
        choice_summaries: list[dict[str, Any]] = []
        for choice in parsed["choices"]:
            if not isinstance(choice, dict):
                choice_summaries.append({"shape": type(choice).__name__})
                continue
            item: dict[str, Any] = {"finish_reason": choice.get("finish_reason")}
            message = choice.get("message")
            delta = choice.get("delta")
            if isinstance(message, dict):
                item["message_role"] = message.get("role")
                item["message_content"] = summarize_content(message.get("content"))
            if isinstance(delta, dict):
                item["delta_keys"] = sorted(str(key) for key in delta.keys())
                if "content" in delta:
                    item["delta_content"] = summarize_content(delta.get("content"))
            choice_summaries.append(item)
        summary["choice_count"] = len(parsed["choices"])
        summary["choices"] = choice_summaries
    if isinstance(parsed.get("error"), dict):
        summary["error_keys"] = sorted(str(key) for key in parsed["error"].keys())
        summary["error_type"] = parsed["error"].get("type") or parsed["error"].get("code")
    return summary


def redacted_case(case: dict[str, Any]) -> dict[str, Any]:
    out = dict(case)
    if "request" in out:
        req = dict(out["request"])
        if "messages" in req:
            req["messages_summary"] = summarize_messages(req.pop("messages"))
        if "system" in req:
            system = req.pop("system")
            req["system_summary"] = {"shape": "text", "text_len": len(str(system))}
        out["request"] = req
    if "response" in out:
        resp = dict(out["response"])
        body = str(resp.pop("body_preview", ""))
        body = body.replace(os.getenv("NEWAPI_API_KEY", "sk-dev"), "[REDACTED]")
        resp["body_summary"] = summarize_response_body(body)
        out["response"] = resp
    return out


def main() -> int:
    parser = argparse.ArgumentParser(description="Run a minimal NewAPI -> ZenProxy smoke and collect evidence.")
    parser.add_argument("--scenario", default="chain-smoke")
    parser.add_argument("--model", default=os.getenv("ZEN_TEST_MODEL", "deepseek-v4-flash"))
    parser.add_argument("--newapi-base-url", default=os.getenv("NEWAPI_BASE_URL", "http://127.0.0.1:8081"))
    parser.add_argument("--newapi-key", default=os.getenv("NEWAPI_API_KEY", "sk-dev"))
    parser.add_argument("--zen-base-url", default=os.getenv("ZEN_BASE_URL", "http://127.0.0.1:4000"))
    parser.add_argument("--zen-admin-base-url", action="append", default=[])
    parser.add_argument("--admin-key", default=os.getenv("ZEN_ADMIN_KEY", "test-key"))
    parser.add_argument("--timeout", type=float, default=90.0)
    parser.add_argument("--out-dir", default="test-records/runs")
    args = parser.parse_args()

    run_id = time.strftime("%Y%m%d-%H%M%S") + f"-{args.scenario}"
    run_dir = Path(args.out_dir) / run_id
    cases_dir = run_dir / "client-cases"
    cases_dir.mkdir(parents=True, exist_ok=True)
    from_ms = now_ms()

    base = args.newapi_base_url.rstrip("/")
    cases: list[dict[str, Any]] = []

    models_result = request_json(f"{base}/v1/models", args.newapi_key, None, args.timeout)
    cases.append({"case_id": "P0-models", "kind": "models", "response": models_result})

    nonstream_payload = {
        "model": args.model,
        "stream": False,
        "messages": [
            {"role": "system", "content": "Reply with one short sentence."},
            {"role": "user", "content": f"Chain smoke run {run_id}: answer OK."},
        ],
    }
    cases.append({
        "case_id": "P0-openai-nonstream",
        "kind": "openai_chat",
        "stream": False,
        "request": nonstream_payload,
        "response": request_json(f"{base}/v1/chat/completions", args.newapi_key, nonstream_payload, args.timeout),
    })

    stream_payload = dict(nonstream_payload)
    stream_payload["stream"] = True
    stream_payload["messages"] = [
        {"role": "system", "content": "Reply with exactly two short words."},
        {"role": "user", "content": f"Chain stream smoke run {run_id}."},
    ]
    cases.append({
        "case_id": "P0-openai-stream",
        "kind": "openai_chat",
        "stream": True,
        "request": stream_payload,
        "response": request_json(f"{base}/v1/chat/completions", args.newapi_key, stream_payload, args.timeout, stream=True),
    })

    malformed_tool_payload = {
        "model": args.model,
        "stream": False,
        "messages": [
            {"role": "user", "content": "Use the supplied tool result as context and say repaired."},
            {
                "role": "assistant",
                "content": None,
                "tool_calls": [
                    {
                        "id": "call_chain_smoke",
                        "type": "function",
                        "function": {"name": "lookup", "arguments": "{\"x\":1}"},
                    }
                ],
            },
            {"role": "tool", "content": "tool result payload intentionally missing tool_call_id"},
            {"role": "user", "content": "Continue."},
        ],
    }
    cases.append({
        "case_id": "P0-openai-missing-tool-call-id",
        "kind": "openai_tool_repair",
        "stream": False,
        "request": malformed_tool_payload,
        "response": request_json(f"{base}/v1/chat/completions", args.newapi_key, malformed_tool_payload, args.timeout),
    })

    anthropic_nonstream_payload = {
        "model": args.model,
        "max_tokens": 64,
        "stream": False,
        "system": "Reply with one short sentence.",
        "messages": [
            {"role": "user", "content": f"Anthropic chain smoke run {run_id}: answer OK."},
        ],
    }
    cases.append({
        "case_id": "P0-anthropic-messages-nonstream",
        "kind": "anthropic_messages",
        "stream": False,
        "request": anthropic_nonstream_payload,
        "response": request_json(f"{base}/v1/messages", args.newapi_key, anthropic_nonstream_payload, args.timeout),
    })

    anthropic_stream_payload = dict(anthropic_nonstream_payload)
    anthropic_stream_payload["stream"] = True
    anthropic_stream_payload["system"] = "Reply with exactly two short words."
    anthropic_stream_payload["messages"] = [
        {"role": "user", "content": f"Anthropic stream smoke run {run_id}."},
    ]
    cases.append({
        "case_id": "P0-anthropic-messages-stream",
        "kind": "anthropic_messages",
        "stream": True,
        "request": anthropic_stream_payload,
        "response": request_json(f"{base}/v1/messages", args.newapi_key, anthropic_stream_payload, args.timeout, stream=True),
    })

    anthropic_missing_tool_use_id_payload = {
        "model": args.model,
        "max_tokens": 64,
        "stream": False,
        "messages": [
            {"role": "user", "content": "Use the supplied tool result as context and say repaired."},
            {
                "role": "assistant",
                "content": [
                    {
                        "type": "tool_use",
                        "id": "toolu_chain_smoke",
                        "name": "lookup",
                        "input": {"x": 1},
                    }
                ],
            },
            {
                "role": "user",
                "content": [
                    {"type": "tool_result", "content": "tool result payload intentionally missing tool_use_id"},
                ],
            },
            {"role": "user", "content": "Continue."},
        ],
    }
    cases.append({
        "case_id": "P0-anthropic-missing-tool-use-id",
        "kind": "anthropic_tool_repair",
        "stream": False,
        "request": anthropic_missing_tool_use_id_payload,
        "response": request_json(f"{base}/v1/messages", args.newapi_key, anthropic_missing_tool_use_id_payload, args.timeout),
    })

    anthropic_mixed_tool_result_payload = {
        "model": args.model,
        "max_tokens": 64,
        "stream": False,
        "messages": [
            {"role": "user", "content": "Use the supplied tool result and answer with a short status."},
            {
                "role": "assistant",
                "content": [
                    {
                        "type": "tool_use",
                        "id": "toolu_chain_smoke_mixed",
                        "name": "lookup",
                        "input": {"target": "mixed"},
                    }
                ],
            },
            {
                "role": "user",
                "content": [
                    {"type": "text", "text": "The tool result follows in the same message."},
                    {
                        "type": "tool_result",
                        "tool_use_id": "toolu_chain_smoke_mixed",
                        "content": "mixed text and tool result payload",
                    },
                ],
            },
        ],
    }
    cases.append({
        "case_id": "P0-anthropic-mixed-text-tool-result",
        "kind": "anthropic_tool_repair",
        "stream": False,
        "request": anthropic_mixed_tool_result_payload,
        "response": request_json(f"{base}/v1/messages", args.newapi_key, anthropic_mixed_tool_result_payload, args.timeout),
    })

    to_ms = now_ms()
    for case in cases:
        (cases_dir / f"{case['case_id']}.json").write_text(
            json.dumps(redacted_case(case), ensure_ascii=False, indent=2) + "\n",
            encoding="utf-8",
        )

    collect_cmd = [
        sys.executable,
        "scripts/collect_test_record.py",
        "--scenario",
        args.scenario,
        "--run-id",
        run_id,
        "--zen-base-url",
        args.zen_base_url,
        "--newapi-base-url",
        args.newapi_base_url,
        "--admin-key",
        args.admin_key,
        "--from-ms",
        str(max(0, from_ms - 5_000)),
        "--to-ms",
        str(to_ms + 5_000),
    ]
    for admin_base_url in args.zen_admin_base_url:
        collect_cmd.extend(["--zen-admin-base-url", admin_base_url])
    subprocess.run(collect_cmd, check=True)

    status_lines = ["# Client Smoke Cases", ""]
    ok = 0
    for case in cases:
        resp = case["response"]
        success = int(resp.get("status") or 0) < 400 and not resp.get("error")
        ok += 1 if success else 0
        status_lines.append(
            f"- {case['case_id']}: status={resp.get('status')} first_byte_ms={resp.get('first_byte_ms')} "
            f"first_content_ms={resp.get('first_content_ms')} total_ms={resp.get('total_ms')} "
            f"body_bytes={resp.get('body_bytes')} error={resp.get('error') or ''}"
        )
    status_lines.extend(["", f"passed_estimated: {ok}/{len(cases)}", ""])
    (run_dir / "client-smoke.md").write_text("\n".join(status_lines), encoding="utf-8")
    with (run_dir / "summary.md").open("a", encoding="utf-8") as fh:
        fh.write("\n")
        fh.write("\n".join(status_lines))
    print(str(run_dir))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
