#!/usr/bin/env python3
"""Programming bulk context gate — HTTP via closeTest NewAPI (same as Pi), bulk IN prompt.

NOT a bypass to model upstream. Requests go to sub2api.closeapi.top (closeTest key)
→ NewAPI channel (e.g. ch109) → panda :4010 → zen-proxy-test :4011.

Achieves 100k/200k/350k prompt_tokens by embedding fixture source in the user
message (Pi read tool cannot). Three rounds per case share one multi-turn session:
load sends bulk once; code_q1/code_q2 append short suffix-only user turns.

Do NOT use panda tailscale :8081 for acceptance — that skips the closeTest channel
routing you care about. Use CLOSETEST_API_KEY + https://sub2api.closeapi.top.
"""

from __future__ import annotations

import argparse
import json
import os
import sys
import time
from pathlib import Path
from typing import Any
from urllib import error, request

_SCRIPT_DIR = Path(__file__).resolve().parent
sys.path.insert(0, str(_SCRIPT_DIR))

from run_pi_matrix_8x5_test import run_ch109_acceptance  # noqa: E402

SPEC_DEFAULT = Path(__file__).resolve().parent / "pi-matrix" / "TASK_SPEC_PROG_BULK.json"
KEY_ENV = (
    "CLOSETEST_API_KEY",
    "NEWAPI_CLOSETEST_KEY",
    "PANDA_NEWAPI_KEY",
    "NEWAPI_API_KEY",
    "OPENAI_API_KEY",
)
DEFAULT_BASE = os.environ.get("NEWAPI_BASE_URL", "https://sub2api.closeapi.top")
DEFAULT_CLIENT_HEADER = os.environ.get("CLOSETEST_X_FMC_CLIENT", "claude-code")


def resolve_spec_root(spec: dict[str, Any]) -> Path:
    root = Path(os.environ.get("PI_PROG_BULK_ROOT", spec["root"]))
    if ":" in str(root):
        normalized = str(root).replace("\\", "/")
        if len(normalized) >= 2 and normalized[1] == ":":
            drive = normalized[0].lower()
            rest = normalized[2:].lstrip("/")
            return Path(f"/mnt/{drive}") / rest
    return root


def load_key() -> str:
    for name in KEY_ENV:
        val = os.environ.get(name)
        if val:
            return val.strip()
    raise SystemExit(
        f"Set closeTest key via one of: {', '.join(KEY_ENV)} "
        f"(same key Pi closeTest provider uses; routes to zen-proxy-test via NewAPI)"
    )


def estimate_tokens(text: str) -> int:
    return max(1, len(text.encode("utf-8")) // 4)


def http_stream_chat(
    base_url: str,
    key: str,
    payload: dict[str, Any],
    timeout_s: int,
    client_header: str = DEFAULT_CLIENT_HEADER,
) -> dict[str, Any]:
    url = base_url.rstrip("/") + "/v1/chat/completions"
    body = json.dumps(payload, ensure_ascii=False).encode("utf-8")
    headers = {
        "Authorization": f"Bearer {key}",
        "Content-Type": "application/json",
        "User-Agent": "claude-code/acceptance-harness",
        "x-fmc-client": client_header,
    }
    started = time.monotonic()
    req = request.Request(url=url, data=body, headers=headers, method="POST")
    opener = request.build_opener(request.ProxyHandler({}))
    text_parts: list[str] = []
    usage: dict[str, Any] = {}
    status = 0
    raw_lines: list[str] = []
    first_byte_ms: int | None = None
    try:
        with opener.open(req, timeout=timeout_s) as resp:
            status = resp.status
            first_byte_ms = int((time.monotonic() - started) * 1000)
            while True:
                line = resp.readline()
                if not line:
                    break
                raw_lines.append(line.decode("utf-8", errors="replace"))
                s = raw_lines[-1].strip()
                if not s.startswith("data:"):
                    continue
                data = s[5:].strip()
                if data == "[DONE]":
                    break
                try:
                    obj = json.loads(data)
                except json.JSONDecodeError:
                    continue
                if obj.get("usage"):
                    usage = obj["usage"]
                delta = (obj.get("choices") or [{}])[0].get("delta") or {}
                if delta.get("content"):
                    text_parts.append(str(delta["content"]))
    except error.HTTPError as exc:
        status = exc.code
        raw_lines.append(exc.read().decode("utf-8", errors="replace"))
    except Exception as exc:  # noqa: BLE001
        raw_lines.append(f"{type(exc).__name__}: {exc}")
    total_ms = int((time.monotonic() - started) * 1000)
    return {
        "status": status,
        "text": "".join(text_parts),
        "usage": usage,
        "first_byte_ms": first_byte_ms,
        "total_ms": total_ms,
        "raw_tail": "\n".join(raw_lines[-20:]),
    }


def verify_turn(turn: dict[str, Any], text: str, est_prompt_tokens: int, min_prompt: int) -> dict[str, Any]:
    blob = text.lower()
    issues: list[str] = []
    if turn.get("marker") and turn["marker"].lower() not in blob:
        issues.append(f"missing marker {turn['marker']}")
    if min_prompt and est_prompt_tokens < min_prompt:
        issues.append(f"prompt est {est_prompt_tokens} < min {min_prompt}")
    q = turn.get("quality") or {}
    for fact in q.get("required_facts", []):
        if fact.lower() not in blob:
            issues.append(f"missing fact: {fact}")
    for token in q.get("required_tokens", []):
        if token.lower() not in blob:
            issues.append(f"missing token: {token}")
    min_chars = int(q.get("min_chars") or 0)
    if min_chars and len(text.strip()) < min_chars:
        issues.append(f"too short: {len(text.strip())} < {min_chars}")
    for phrase in q.get("forbidden_phrases", []):
        if phrase.lower() in blob:
            issues.append(f"forbidden: {phrase}")
    return {
        "turn_id": turn["id"],
        "pass": not issues,
        "issues": issues,
        "answer_chars": len(text.strip()),
        "est_prompt_tokens": est_prompt_tokens,
    }


def summarize_bucket_cache(run_dir: Path) -> dict[str, Any]:
    """Per-tier cache stats from turn-*.meta.json (API billing + client prompt)."""
    raw = run_dir / "raw"
    if not raw.is_dir():
        return {}
    rows: list[dict[str, Any]] = []
    for meta in sorted(raw.rglob("turn-*.meta.json")):
        data = json.loads(meta.read_text(encoding="utf-8"))
        usage = data.get("usage") or {}
        billing = (usage.get("billing_usage") or {}).get("claude_usage") or {}
        pt = int(usage.get("prompt_tokens") or 0)
        inp = int(billing.get("input_tokens") or 0)
        cr = int(billing.get("cache_read_input_tokens") or 0)
        out = int(billing.get("output_tokens") or usage.get("completion_tokens") or 0)
        case_id = meta.parent.name
        turn_id = meta.name.replace("turn-", "").replace(".meta.json", "")
        tier = case_id.replace("bucket_", "").replace("prog_", "")
        rows.append(
            {
                "case_id": case_id,
                "tier": tier,
                "turn_id": turn_id,
                "prompt_tokens": pt,
                "input_tokens": inp,
                "cache_read": cr,
                "output_tokens": out,
            }
        )
    tiers: dict[str, list[dict[str, Any]]] = {}
    for row in rows:
        tiers.setdefault(row["tier"], []).append(row)

    def _agg(label: str, subset: list[dict[str, Any]]) -> dict[str, Any]:
        pt = sum(r["prompt_tokens"] for r in subset)
        inp = sum(r["input_tokens"] for r in subset)
        cr = sum(r["cache_read"] for r in subset)
        out = sum(r["output_tokens"] for r in subset)
        total_in = inp + cr
        return {
            "label": label,
            "n": len(subset),
            "prompt_sum": pt,
            "input_sum": inp,
            "cache_read_sum": cr,
            "output_sum": out,
            "provider_cache_pct": round(100.0 * cr / total_in, 2) if total_in else 0.0,
            "client_cache_read_over_prompt_pct": round(100.0 * cr / pt, 2) if pt else 0.0,
        }

    by_tier = {tier: _agg(f"tier_{tier}", items) for tier, items in sorted(tiers.items())}
    by_turn: dict[str, dict[str, Any]] = {}
    for turn_id in ("load", "code_q1", "code_q2"):
        subset = [r for r in rows if r["turn_id"] == turn_id]
        if subset:
            by_turn[turn_id] = _agg(f"turn_{turn_id}", subset)
    return {
        "all": _agg("all", rows),
        "by_tier": by_tier,
        "by_turn": by_turn,
        "rows": rows,
    }


def run_case(
    case: dict[str, Any],
    root: Path,
    base_url: str,
    key: str,
    model: str,
    timeout_s: int,
    min_prompt_map: dict[str, int],
    run_dir: Path,
    client_header: str,
    max_tokens: int,
) -> dict[str, Any]:
    src = root / case["dir"] / case["module"]
    bulk = src.read_text(encoding="utf-8", errors="replace")
    prefix = f"以下是需要通读的大型 Rust 模块源码（测试夹具）：\n\n```rust\n{bulk}\n```\n\n"
    tier = str(case["target_context_tokens"])
    min_prompt = int(min_prompt_map.get(tier, 0))
    turn_rows: list[dict[str, Any]] = []
    case_out = run_dir / "raw" / case["id"]
    case_out.mkdir(parents=True, exist_ok=True)

    messages: list[dict[str, Any]] = []
    for turn in case["turns"]:
        if turn["id"] == "load":
            user_content = prefix + turn["suffix"]
        else:
            user_content = turn["suffix"]
        messages.append({"role": "user", "content": user_content})
        est = estimate_tokens(prefix + turn["suffix"])
        payload = {
            "model": model,
            "stream": True,
            "max_tokens": max_tokens,
            "messages": [dict(m) for m in messages],
        }
        resp = http_stream_chat(base_url, key, payload, timeout_s, client_header=client_header)
        (case_out / f"turn-{turn['id']}.stdout").write_text(resp["text"], encoding="utf-8")
        (case_out / f"turn-{turn['id']}.meta.json").write_text(
            json.dumps(resp, indent=2, ensure_ascii=False) + "\n", encoding="utf-8"
        )
        usage_prompt = int((resp.get("usage") or {}).get("prompt_tokens") or 0)
        check_tokens = usage_prompt if usage_prompt > 0 else est
        row = verify_turn(turn, resp["text"], check_tokens, min_prompt if turn["id"] == "load" else 0)
        row["api_status"] = resp["status"]
        row["usage_prompt_tokens"] = usage_prompt
        row["first_byte_ms"] = resp.get("first_byte_ms")
        row["total_ms"] = resp.get("total_ms")
        turn_rows.append(row)
        print(json.dumps({"event": "turn", "case_id": case["id"], **row}, ensure_ascii=False))
        messages.append({"role": "assistant", "content": resp["text"]})

    return {
        "case_id": case["id"],
        "target_context_tokens": case["target_context_tokens"],
        "turns": turn_rows,
        "pass": all(t["pass"] for t in turn_rows) and all(t.get("api_status") == 200 for t in turn_rows),
    }


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--spec", default=str(SPEC_DEFAULT))
    parser.add_argument("--base-url", default=DEFAULT_BASE)
    parser.add_argument("--model", default="deepseek-v4-flash")
    parser.add_argument("--max-tokens", type=int, default=2048)
    parser.add_argument("--timeout-s", type=int, default=600)
    parser.add_argument(
        "--client-header",
        default=DEFAULT_CLIENT_HEADER,
        help="x-fmc-client for NewAPI/ZenProxy profile (Pi closeTest uses claude-code)",
    )
    parser.add_argument("--run-dir", default="")
    parser.add_argument("--run-ch109-acceptance", action="store_true")
    parser.add_argument("--ch109-label", default="prog-bulk-direct")
    args = parser.parse_args(argv)

    spec = json.loads(Path(args.spec).read_text(encoding="utf-8"))
    root = resolve_spec_root(spec)
    if not root.exists():
        print("run setup_prog_bulk_fixtures.py or setup_cache_bucket_fixtures.py first", file=sys.stderr)
        return 2

    key = load_key()
    stamp = time.strftime("%Y%m%d-%H%M%S")
    run_dir = Path(args.run_dir) if args.run_dir else _SCRIPT_DIR / ".local-dev" / "runs" / f"prog-bulk-direct-{stamp}"
    run_dir.mkdir(parents=True, exist_ok=True)
    (run_dir / "route.json").write_text(
        json.dumps(
            {
                "base_url": args.base_url,
                "route": "closeTest NewAPI -> channel (key-bound) -> panda :4010 -> zen-proxy-test :4011",
                "x_fmc_client": args.client_header,
                "model": args.model,
                "ch109_metrics_source": "run_ch109_acceptance_window.py channel_id=109",
            },
            indent=2,
        )
        + "\n",
        encoding="utf-8",
    )

    wall_start = int(time.time())
    t0 = time.monotonic()
    cases_out = [
        run_case(
            case,
            root,
            args.base_url,
            key,
            args.model,
            args.timeout_s,
            spec.get("min_prompt_tokens", {}),
            run_dir,
            args.client_header,
            args.max_tokens,
        )
        for case in spec["cases"]
    ]
    wall_ms = int((time.monotonic() - t0) * 1000)
    wall_end = int(time.time())

    report = {
        "run_dir": str(run_dir),
        "mode": "closetest-newapi-http-bulk-in-prompt",
        "cases": cases_out,
        "pass_count": sum(1 for c in cases_out if c["pass"]),
        "gate_pass": all(c["pass"] for c in cases_out),
    }
    (run_dir / "quality_gate.json").write_text(json.dumps(report, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")

    ch109 = None
    if args.run_ch109_acceptance:
        ch109 = run_ch109_acceptance(run_dir, wall_start, wall_end, args.ch109_label)

    bucket_cache = summarize_bucket_cache(run_dir)
    if bucket_cache:
        (run_dir / "bucket_cache.json").write_text(
            json.dumps(bucket_cache, indent=2, ensure_ascii=False) + "\n", encoding="utf-8"
        )

    final = {
        "gate_pass": report["gate_pass"],
        "wall_ms": wall_ms,
        "pass_count": report["pass_count"],
        "ch109_cache_pct": (ch109 or {}).get("newapi", {}).get("cache_pct_token_weighted"),
        "ch109_frt_ms": (ch109 or {}).get("newapi", {}).get("frt_ms"),
        "bucket_cache": {
            "all": bucket_cache.get("all"),
            "by_tier": bucket_cache.get("by_tier"),
        },
    }
    (run_dir / "final.json").write_text(json.dumps(final, indent=2) + "\n", encoding="utf-8")
    print(json.dumps({"event": "done", **final}, ensure_ascii=False))
    return 0 if report["gate_pass"] else 1


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
