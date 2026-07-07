#!/usr/bin/env python3
"""Render the 2026-07-07 ClaudeCode/opencode rerun report."""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parents[2]
CC_RUN = ROOT / ".local-dev/runs/newapi-dualhost-project-matrix-20260707-claudecode-dualhost-rerun"
OP_RUN = ROOT / ".local-dev/runs/opencode-native-project-matrix-20260707-opencode-native-rerun"
OUT = ROOT / ".local-dev/runs/20260707-cc-opencode-rerun-report.md"


def result_path(value: str) -> Path:
    prefix = "\\\\wsl.localhost\\HermesUbuntu\\home\\lenovo\\zen-free-model-suite\\"
    if value.startswith(prefix):
        return ROOT / value[len(prefix) :].replace("\\", "/")
    return Path(value)


def pct(numerator: int, denominator: int) -> float | None:
    return None if denominator <= 0 else round(numerator / denominator * 100, 2)


def fmt(value: Any) -> str:
    if value is None:
        return "n/a"
    if isinstance(value, float):
        return f"{value:.2f}"
    return str(value)


def seconds(ms: int) -> float:
    return round(ms / 1000.0, 1)


def parse_cc_stdout(path: str) -> dict[str, Any]:
    final: dict[str, Any] | None = None
    assistant_chars = 0
    tool_events = 0
    api_retries = 0
    for line in result_path(path).read_text(encoding="utf-8", errors="replace").splitlines():
        try:
            event = json.loads(line)
        except json.JSONDecodeError:
            continue
        if event.get("type") == "result":
            final = event
        if event.get("type") == "assistant":
            message = event.get("message") if isinstance(event.get("message"), dict) else {}
            for content in message.get("content") or []:
                if isinstance(content, dict) and isinstance(content.get("text"), str):
                    assistant_chars += len(content["text"])
        if event.get("tool_use_result") is not None or event.get("type") in {"tool_use", "tool_result"}:
            tool_events += 1
        if event.get("subtype") == "api_retry":
            api_retries += 1
    if not final:
        return {
            "has_result": False,
            "is_error": True,
            "api": None,
            "result_len": 0,
            "assistant_chars": assistant_chars,
            "tool_events": tool_events,
            "turns": None,
            "input": 0,
            "read": 0,
            "creation": 0,
            "output": 0,
            "api_retries": api_retries,
        }
    usage = final.get("usage") if isinstance(final.get("usage"), dict) else {}
    return {
        "has_result": True,
        "is_error": bool(final.get("is_error")),
        "api": final.get("api_error_status"),
        "result_len": len(str(final.get("result") or "")),
        "assistant_chars": assistant_chars,
        "tool_events": tool_events,
        "turns": final.get("num_turns"),
        "input": int(usage.get("input_tokens") or 0),
        "read": int(usage.get("cache_read_input_tokens") or 0),
        "creation": int(usage.get("cache_creation_input_tokens") or 0),
        "output": int(usage.get("output_tokens") or 0),
        "api_retries": api_retries,
    }


def opencode_text(stdout_path: str) -> str:
    parts: list[str] = []
    for line in result_path(stdout_path).read_text(encoding="utf-8", errors="replace").splitlines():
        try:
            event = json.loads(line)
        except json.JSONDecodeError:
            continue
        part = event.get("part") if isinstance(event.get("part"), dict) else {}
        if event.get("type") == "text" and isinstance(part.get("text"), str):
            parts.append(part["text"])
    return "\n".join(parts)


def render() -> None:
    cc = json.loads((CC_RUN / "summary.json").read_text(encoding="utf-8"))
    op = json.loads((OP_RUN / "results.json").read_text(encoding="utf-8"))["results"]
    audit = json.loads((CC_RUN / "audit-window-summary.json").read_text(encoding="utf-8"))

    cc_rows: list[dict[str, Any]] = []
    for model_summary in cc:
        for row in model_summary["results"]:
            parsed = parse_cc_stdout(row["stdout_path"])
            status = "timeout" if row["timeout"] else str(row["exit_code"])
            if parsed["has_result"] and parsed["is_error"] and parsed["api"]:
                status += f"/api{parsed['api']}"
            note = ""
            if row["exit_code"] == 0 and not parsed["is_error"]:
                quality = 4 if parsed["result_len"] >= 4000 else 3
            elif parsed["api"] == 502:
                quality = 0
                note = "API 502 after retries"
            elif not parsed["has_result"]:
                quality = 1
                note = "no final result event"
            else:
                quality = 1
            cc_rows.append({**row, **parsed, "status": status, "quality": quality, "note": note})

    op_rows: list[dict[str, Any]] = []
    for row in op:
        text = opencode_text(row["stdout_path"])
        quality = 4 if row["text_chars"] >= 2500 and row["exit_code"] == 0 and not row["timeout"] else 2
        safety = ""
        if row["model"] == "deepseek-v4-flash" and row["case_id"] == "outlook-register" and "Summary of changes" in text:
            quality = 1
            safety = "violated read-only; confirmed tracked diff restored"
        elif "只读" in text or "read-only" in text.lower():
            safety = "claims read-only"
        op_rows.append(
            {
                **row,
                "cache_share": pct(row["cache_read_tokens"], row["cache_read_tokens"] + row["input_tokens"]),
                "quality": quality,
                "safety": safety,
            }
        )

    lines: list[str] = [
        "# 2026-07-07 ClaudeCode / opencode 三模型复测报告",
        "",
        f"- ClaudeCode run: `{CC_RUN}`",
        f"- opencode run: `{OP_RUN}`",
        "- 执行方式：三模型串行；每个模型内 4 个问题并发。ClaudeCode 为 Windows 4 路 + WSL 4 路并发；opencode 原生为 Windows 4 路并发。",
        "- 安全修复：opencode DeepSeek/outlook-register 曾违反只读 prompt 并改动 tracked 文件；已定向恢复，误写 diff 备份在 `opencode-native-project-matrix-20260707-opencode-native-rerun/safety-backups/autoregister-confirmed-opencode-write.diff`。",
        "",
        "## 一、总览",
        "",
        "| 路径 | 模型 | 成功/总数 | 批次 wall time(s) | 主要结论 |",
        "|---|---|---:|---:|---|",
    ]

    for model_summary in cc:
        rows = [item for item in cc_rows if item["model"] == model_summary["model"]]
        ok = sum(1 for item in rows if item["exit_code"] == 0 and not item["is_error"])
        if model_summary["model"] == "deepseek-v4-flash":
            conclusion = "Windows 无 result；WSL 全 502；provider R2 低"
        elif model_summary["model"] == "mimo-v2.5":
            conclusion = "Windows 4/4 成功；WSL 全 502；provider audit 归因失真"
        else:
            conclusion = "Windows 4/4 成功；WSL 全 502；provider audit 缺失/被 DeepSeek 污染"
        lines.append(
            f"| ClaudeCode dualhost | {model_summary['model']} | {ok}/8 | {model_summary['elapsed_s']} | {conclusion} |"
        )

    for model in ["deepseek-v4-flash", "mimo-v2.5", "big-pickle"]:
        rows = [item for item in op_rows if item["model"] == model]
        ok = sum(1 for item in rows if item["exit_code"] == 0 and not item["timeout"])
        wall = max((item["elapsed_ms"] for item in rows), default=0) / 1000.0
        if model == "deepseek-v4-flash":
            conclusion = "4/4 exit 0，但 outlook-register 严重偏航并误写"
        elif model == "mimo-v2.5":
            conclusion = "4/4 exit 0，报告较稳"
        else:
            conclusion = "4/4 exit 0，Tide 最慢但输出最详细"
        lines.append(f"| opencode native | {model} | {ok}/4 | {wall:.1f} | {conclusion} |")

    lines += [
        "",
        "## 二、ClaudeCode Per-case",
        "",
        "| model | platform | case | status | elapsed_s | stdout_kb | result_chars | turns | tools | client cache read | client input | read/(input+read) | quality | note |",
        "|---|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---|",
    ]
    for row in sorted(cc_rows, key=lambda item: (item["model"], item["platform"], item["case_id"])):
        share = pct(row["read"], row["read"] + row["input"] + row["creation"])
        turns = row["turns"] if row["turns"] is not None else ""
        lines.append(
            f"| {row['model']} | {row['platform']} | {row['case_id']} | {row['status']} | {seconds(row['elapsed_ms'])} | "
            f"{row['stdout_bytes'] / 1024:.1f} | {row['result_len']} | {turns} | {row['tool_events']} | "
            f"{row['read']} | {row['input']} | {fmt(share)}% | {row['quality']} | {row['note']} |"
        )

    lines += [
        "",
        "## 三、ClaudeCode Provider Audit",
        "",
        "| model window | rows | R2 read/(read+miss) | read | miss | outcomes | observations | pin_hit | raw_prefix_match | unique_usk | note |",
        "|---|---:|---:|---:|---:|---|---|---:|---:|---:|---|",
    ]
    for model, summary in audit["summary"].items():
        if model == "mimo-v2.5":
            note = "仅 2 条 tiny probe；同窗口另有 47 条 DeepSeek，不能代表 Mimo"
        elif model == "big-pickle":
            note = "0 条匹配；同窗口 61 条 DeepSeek，不能代表 BigPickle"
        else:
            note = "真实匹配；raw prefix 32k match=0%，cache identity 不稳"
        lines.append(
            f"| {model} | {summary['rows']} | {fmt(summary['r2_pct'])}% | {summary['read']} | {summary['miss']} | "
            f"{summary['outcomes']} | {summary['observations']} | {fmt(summary['pin_hit_pct'])}% | "
            f"{fmt(summary['raw_prefix_match_pct'])}% | {summary['unique_usk']} | {note} |"
        )
    lines += ["", "窗口内模型计数：", ""]
    for model, counts in audit["window_model_counts"].items():
        lines.append(f"- {model}: `{counts}`")

    lines += [
        "",
        "## 四、opencode Native Per-case",
        "",
        "| model | case | exit | elapsed_s | text_chars | events | tool_events | input | output | reasoning | cache_read | cache_write | read/(input+read) | quality | safety |",
        "|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---|",
    ]
    for row in sorted(op_rows, key=lambda item: (item["model"], item["case_id"])):
        exit_value = "timeout" if row["timeout"] else row["exit_code"]
        lines.append(
            f"| {row['model']} | {row['case_id']} | {exit_value} | {seconds(row['elapsed_ms'])} | "
            f"{row['text_chars']} | {row['event_count']} | {row['tool_event_count']} | {row['input_tokens']} | "
            f"{row['output_tokens']} | {row['reasoning_tokens']} | {row['cache_read_tokens']} | "
            f"{row['cache_write_tokens']} | {fmt(row['cache_share'])}% | {row['quality']} | {row['safety']} |"
        )

    lines += [
        "",
        "## 五、聚合 Cache",
        "",
        "| path | model | read | input/miss basis | cache pct | caveat |",
        "|---|---|---:|---:|---:|---|",
    ]
    for model in ["mimo-v2.5", "big-pickle"]:
        rows = [
            item
            for item in cc_rows
            if item["model"] == model and item["platform"] == "windows" and item["exit_code"] == 0
        ]
        read = sum(item["read"] for item in rows)
        basis = sum(item["input"] + item["read"] + item["creation"] for item in rows)
        lines.append(
            f"| ClaudeCode Windows client usage | {model} | {read} | {basis} | {fmt(pct(read, basis))}% | CLI result usage, not provider audit |"
        )
    for model, summary in audit["summary"].items():
        if summary["rows"]:
            caveat = "valid only for DeepSeek window" if model == "deepseek-v4-flash" else "invalid attribution"
            lines.append(
                f"| ClaudeCode panda provider audit | {model} | {summary['read']} | {summary['read'] + summary['miss']} | {fmt(summary['r2_pct'])}% | {caveat} |"
            )
    for model in ["deepseek-v4-flash", "mimo-v2.5", "big-pickle"]:
        rows = [item for item in op_rows if item["model"] == model]
        read = sum(item["cache_read_tokens"] for item in rows)
        basis = sum(item["cache_read_tokens"] + item["input_tokens"] for item in rows)
        lines.append(
            f"| opencode native event tokens | {model} | {read} | {basis} | {fmt(pct(read, basis))}% | opencode token event semantics; cache_write=0 throughout |"
        )

    lines += [
        "",
        "## 六、质量判断",
        "",
        "- ClaudeCode Windows：Mimo 与 BigPickle 都完成 4/4，输出是正常项目审计/清理报告；DeepSeek 4/4 exit=1 且无 final result，质量失败。",
        "- ClaudeCode WSL：三模型 12/12 都是 API 502 after retries，质量为 0；stderr 另有 workspace trust warning，但 stdout 的最终失败根因是 502。",
        "- opencode native：12/12 exit=0，但 DeepSeek `outlook-register` 明确偏离只读审计，执行了修 bug/删跟踪等动作；这一路径不能用 `--dangerously-skip-permissions` 做只读验收。",
        "- opencode Mimo/BigPickle 大多数输出是结构化审计报告；BigPickle Tide 最详细但最慢（1413.8s）。",
        "",
        "## 七、ZenProxy 最小请求单位建议",
        "",
        "1. **不要把最小验收单位定为单个 HTTP request。** ClaudeCode 的实际缓存表现由一个 session window 内的 tool history、Task 子会话、hook 注入、raw body prefix 和 node pin 共同决定；DeepSeek provider audit 里 `unique_usk=8`、`raw_prefix_match=0%`、R2 只有 26.30%。",
        "2. **必须引入 session-window 稳定性指标。** 每个 case/run 需要固定 `run_id/platform/case/session_id/provider_id/public_model/prompt_cache_key/prefix_32k_hash/raw_body_prefix_32k_hash`，并记录 turn index；否则 Mimo/BigPickle 这种窗口内出现 DeepSeek audit 的情况无法归因。",
        "3. **代理稳定性要作为 cache 前置门槛。** WSL 12/12 502，说明在讨论命中率前要先保证 selected node、egress、session pin 和 bad-node quarantine；502/empty_output 后应跨本请求和本 session 立即 unpin/quarantine。",
        "4. **provider 身份稳定性要强制对账。** cc-switch provider、ClaudeCode `--model`、NewAPI/sub2api、ZenProxy audit 的 `public_model/model/upstream_model` 必须一一一致；不一致时该窗口禁止产出缓存达标结论。",
        "5. **缓存体稳定性要看 raw prefix，而不只看 USK。** DeepSeek 窗口 `ccp_raw_prefix_match_32k=0%`，说明工具历史/动态内容仍进入 provider raw body；需要继续做工具 schema freeze、tool_result 动态段隔离、hook/历史摘要隔离。",
        "6. **测试 harness 要有写保护。** opencode native 应改成只读权限模型或沙箱复制目录，不能再用 `--dangerously-skip-permissions` 直接打真实项目。",
        "",
    ]

    OUT.write_text("\n".join(lines), encoding="utf-8")
    print(OUT)


if __name__ == "__main__":
    render()
