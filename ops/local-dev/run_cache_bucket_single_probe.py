#!/usr/bin/env python3
"""Per-tier cache probe: warmup then one measured multi-turn request (suffix-only probe turn).

Gate metric (only): NewAPI / CCSwitch — cache_tokens / prompt_tokens on the probe response.
  cache_tokens = usage.prompt_tokens_details.cached_tokens (what NewAPI logs as other.cache_tokens)
  prompt_tokens  = usage.prompt_tokens

No alternate billing or delta formulas for pass/fail.
"""

from __future__ import annotations

import argparse
import json
import sys
import time
from pathlib import Path
from typing import Any

_SCRIPT_DIR = Path(__file__).resolve().parent
sys.path.insert(0, str(_SCRIPT_DIR))

from run_prog_bulk_context_gate import (  # noqa: E402
    DEFAULT_BASE,
    DEFAULT_CLIENT_HEADER,
    estimate_tokens,
    http_stream_chat,
    load_key,
    resolve_spec_root,
    run_ch109_acceptance,
    summarize_bucket_cache,
)

SPEC_DEFAULT = Path(__file__).resolve().parent / "pi-matrix" / "TASK_SPEC_CACHE_BUCKET.json"
GATE_METRIC = "newapi_cache_tokens_over_prompt_pct"
MIN_NEWAPI_CACHE_PCT = 97.0


def usage_newapi_cache_stats(usage: dict[str, Any]) -> dict[str, Any]:
    """NewAPI / CCSwitch cache hit: cache_tokens / prompt_tokens from stream usage."""
    pt = int(usage.get("prompt_tokens") or 0)
    details = usage.get("prompt_tokens_details") or {}
    cache_tokens = int(details.get("cached_tokens") or 0)
    out = int(usage.get("completion_tokens") or 0)
    billing = (usage.get("billing_usage") or {}).get("claude_usage") or {}
    return {
        "prompt_tokens": pt,
        "cache_tokens": cache_tokens,
        "newapi_cache_pct": round(100.0 * cache_tokens / pt, 2) if pt else 0.0,
        "output_tokens": out,
        # diagnostic only — not used for gate_pass
        "billing_input_tokens": int(billing.get("input_tokens") or 0),
        "billing_cache_read_input_tokens": int(billing.get("cache_read_input_tokens") or 0),
        "billing_cache_creation_input_tokens": int(
            billing.get("cache_creation_input_tokens") or 0
        ),
    }


def run_tier_probe(
    case: dict[str, Any],
    root: Path,
    base_url: str,
    key: str,
    model: str,
    timeout_s: int,
    run_dir: Path,
    client_header: str,
    max_tokens: int,
    min_prompt_map: dict[str, int],
    min_newapi_cache_pct: float,
) -> dict[str, Any]:
    src = root / case["dir"] / case["module"]
    bulk = src.read_text(encoding="utf-8", errors="replace")
    prefix = f"以下是需要通读的大型 Rust 模块源码（测试夹具）：\n\n```rust\n{bulk}\n```\n\n"
    tier = str(case["target_context_tokens"])
    min_prompt = int(min_prompt_map.get(tier, 0))
    case_out = run_dir / "raw" / case["id"]
    case_out.mkdir(parents=True, exist_ok=True)

    warmup_suffix = "预热缓存。只回复一行：CACHE_WARMUP_OK"
    probe_suffix = "单次探测：基于上文回答「CACHE_PROBE_OK」只输出这一行，不要解释。"

    def one_shot(label: str, suffix: str, retries: int = 1) -> dict[str, Any]:
        content = prefix + suffix
        payload = {
            "model": model,
            "stream": True,
            "max_tokens": max_tokens,
            "messages": [{"role": "user", "content": content}],
        }
        resp: dict[str, Any] = {}
        for attempt in range(retries + 1):
            resp = http_stream_chat(
                base_url, key, payload, timeout_s, client_header=client_header
            )
            if resp["status"] == 200 or attempt >= retries:
                break
            time.sleep(min(30, 5 * (attempt + 1)))
        (case_out / f"{label}.stdout").write_text(resp["text"], encoding="utf-8")
        usage = resp.get("usage") or {}
        stats = usage_newapi_cache_stats(usage)
        meta = {
            "status": resp["status"],
            "text": resp["text"],
            "usage": usage,
            "first_byte_ms": resp.get("first_byte_ms"),
            "total_ms": resp.get("total_ms"),
            "gate_metric": GATE_METRIC,
            **stats,
        }
        (case_out / f"{label}.meta.json").write_text(
            json.dumps(meta, indent=2, ensure_ascii=False) + "\n", encoding="utf-8"
        )
        return meta

    warmup = one_shot("warmup", warmup_suffix, retries=2)

    probe_payload = {
        "model": model,
        "stream": True,
        "max_tokens": max_tokens,
        "messages": [
            {"role": "user", "content": prefix + warmup_suffix},
            {"role": "assistant", "content": warmup.get("text") or "CACHE_WARMUP_OK"},
            {"role": "user", "content": probe_suffix},
        ],
    }
    resp = http_stream_chat(
        base_url, key, probe_payload, timeout_s, client_header=client_header
    )
    (case_out / "probe.stdout").write_text(resp["text"], encoding="utf-8")
    usage = resp.get("usage") or {}
    stats = usage_newapi_cache_stats(usage)
    probe = {
        "status": resp["status"],
        "text": resp["text"],
        "usage": usage,
        "first_byte_ms": resp.get("first_byte_ms"),
        "total_ms": resp.get("total_ms"),
        "gate_metric": GATE_METRIC,
        **stats,
    }
    (case_out / "probe.meta.json").write_text(
        json.dumps(probe, indent=2, ensure_ascii=False) + "\n", encoding="utf-8"
    )

    probe_prompt = probe["prompt_tokens"]
    prompt_ok = probe_prompt >= min_prompt if min_prompt else True
    warmup_ok = warmup.get("status") == 200 and int(warmup.get("prompt_tokens") or 0) > 0
    newapi_pct = float(probe["newapi_cache_pct"])
    cache_ok = newapi_pct >= min_newapi_cache_pct if warmup_ok else False
    api_ok = probe["status"] == 200
    marker_ok = "CACHE_PROBE" in (probe.get("text") or "").upper()

    issues: list[str] = []
    if not api_ok:
        issues.append(f"probe http {probe['status']}")
    if not warmup_ok:
        issues.append(f"warmup not ready http {warmup.get('status')}")
    if min_prompt and not prompt_ok:
        issues.append(f"probe prompt {probe_prompt} < min {min_prompt}")
    if warmup_ok and not cache_ok:
        issues.append(
            f"newapi cache_tokens/prompt {newapi_pct}% < {min_newapi_cache_pct}%"
        )
    if not marker_ok:
        issues.append("probe missing CACHE_PROBE marker in text")

    row = {
        "case_id": case["id"],
        "tier": tier,
        "target_context_tokens": case["target_context_tokens"],
        "gate_metric": GATE_METRIC,
        "warmup": warmup,
        "probe": probe,
        "pass": not issues,
        "issues": issues,
    }
    print(json.dumps({"event": "tier_probe", **row}, ensure_ascii=False))
    return row


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--spec", default=str(SPEC_DEFAULT))
    parser.add_argument("--base-url", default=DEFAULT_BASE)
    parser.add_argument("--model", default="deepseek-v4-flash")
    parser.add_argument("--max-tokens", type=int, default=128)
    parser.add_argument("--timeout-s", type=int, default=600)
    parser.add_argument("--client-header", default=DEFAULT_CLIENT_HEADER)
    parser.add_argument("--run-dir", default="")
    parser.add_argument(
        "--min-newapi-cache-pct",
        type=float,
        default=MIN_NEWAPI_CACHE_PCT,
        help="Gate: cache_tokens/prompt_tokens on probe (NewAPI / CCSwitch)",
    )
    parser.add_argument(
        "--min-provider-cache-pct",
        type=float,
        dest="min_newapi_cache_pct",
        help="Deprecated alias for --min-newapi-cache-pct",
    )
    parser.add_argument("--run-ch109-acceptance", action="store_true")
    parser.add_argument("--ch109-label", default="cache-bucket-single-probe")
    args = parser.parse_args(argv)

    spec = json.loads(Path(args.spec).read_text(encoding="utf-8"))
    root = resolve_spec_root(spec)
    if not root.exists():
        print("run setup_cache_bucket_fixtures.py first", file=sys.stderr)
        return 2

    key = load_key()
    stamp = time.strftime("%Y%m%d-%H%M%S")
    run_dir = (
        Path(args.run_dir)
        if args.run_dir
        else _SCRIPT_DIR / ".local-dev" / "runs" / f"cache-bucket-single-{stamp}"
    )
    run_dir.mkdir(parents=True, exist_ok=True)
    (run_dir / "route.json").write_text(
        json.dumps(
            {
                "base_url": args.base_url,
                "mode": "warmup-then-multiturn-probe-suffix-only",
                "route": "closeTest NewAPI -> ch109 -> panda :4010 -> zen-proxy-test :4011",
                "x_fmc_client": args.client_header,
                "model": args.model,
                "gate_metric": GATE_METRIC,
                "min_newapi_cache_pct": args.min_newapi_cache_pct,
            },
            indent=2,
        )
        + "\n",
        encoding="utf-8",
    )

    wall_start = int(time.time())
    t0 = time.monotonic()
    tiers_out = [
        run_tier_probe(
            case,
            root,
            args.base_url,
            key,
            args.model,
            args.timeout_s,
            run_dir,
            args.client_header,
            args.max_tokens,
            spec.get("min_prompt_tokens", {}),
            args.min_newapi_cache_pct,
        )
        for case in spec["cases"]
    ]
    wall_ms = int((time.monotonic() - t0) * 1000)
    wall_end = int(time.time())

    probe_rows = []
    for t in tiers_out:
        p = t["probe"]
        probe_rows.append(
            {
                "case_id": t["case_id"],
                "tier": t["tier"],
                "turn_id": "probe",
                "prompt_tokens": p["prompt_tokens"],
                "cache_tokens": p["cache_tokens"],
                "newapi_cache_pct": p["newapi_cache_pct"],
                "output_tokens": p["output_tokens"],
            }
        )

    report = {
        "run_dir": str(run_dir),
        "mode": "warmup-then-multiturn-probe",
        "gate_metric": GATE_METRIC,
        "min_newapi_cache_pct": args.min_newapi_cache_pct,
        "tiers": tiers_out,
        "pass_count": sum(1 for t in tiers_out if t["pass"]),
        "gate_pass": all(t["pass"] for t in tiers_out),
        "probe_summary": probe_rows,
    }
    (run_dir / "single_probe_gate.json").write_text(
        json.dumps(report, indent=2, ensure_ascii=False) + "\n", encoding="utf-8"
    )

    for t in tiers_out:
        case_out = run_dir / "raw" / t["case_id"]
        probe_meta = json.loads((case_out / "probe.meta.json").read_text(encoding="utf-8"))
        (case_out / "turn-probe.meta.json").write_text(
            json.dumps(probe_meta, indent=2, ensure_ascii=False) + "\n", encoding="utf-8"
        )
    bucket_cache = summarize_bucket_cache(run_dir)
    (run_dir / "bucket_cache.json").write_text(
        json.dumps(bucket_cache, indent=2, ensure_ascii=False) + "\n", encoding="utf-8"
    )

    ch109 = None
    if args.run_ch109_acceptance:
        ch109 = run_ch109_acceptance(run_dir, wall_start, wall_end, args.ch109_label)

    final = {
        "gate_pass": report["gate_pass"],
        "gate_metric": GATE_METRIC,
        "wall_ms": wall_ms,
        "pass_count": report["pass_count"],
        "min_newapi_cache_pct": args.min_newapi_cache_pct,
        "ch109_cache_pct": (ch109 or {}).get("newapi", {}).get("cache_pct_token_weighted"),
        "ch109_frt_ms": (ch109 or {}).get("newapi", {}).get("frt_ms"),
        "probe_by_tier": {
            t["tier"]: {
                "newapi_cache_pct": t["probe"]["newapi_cache_pct"],
                "cache_tokens": t["probe"]["cache_tokens"],
                "prompt_tokens": t["probe"]["prompt_tokens"],
                "pass": t["pass"],
            }
            for t in tiers_out
        },
    }
    (run_dir / "final.json").write_text(json.dumps(final, indent=2) + "\n", encoding="utf-8")
    print(json.dumps({"event": "done", **final}, ensure_ascii=False))
    return 0 if report["gate_pass"] else 1


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
