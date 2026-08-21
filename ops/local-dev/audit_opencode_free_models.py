#!/usr/bin/env python3
"""Compare OpenCode upstream free models vs Zenproxy static/dynamic exposure."""
from __future__ import annotations

import json
import subprocess
import urllib.request

UPSTREAM = "https://opencode.ai/zen/v1/models"

STATIC_PUBLIC = {
    "deepseek-v4-flash": "deepseek-v4-flash-free",
    "big-pickle": "big-pickle",
    "mimo-v2.5": "mimo-v2.5-free",
    "hy3": "hy3-free",
}
RESERVED_UPSTREAM = {"deepseek-v4-flash-free", "big-pickle", "mimo-v2.5-free", "hy3-free"}


def is_free_candidate(model_id: str) -> bool:
    return model_id == "big-pickle" or model_id.endswith("-free")


def public_alias(upstream_id: str) -> str | None:
    if upstream_id.endswith("-free"):
        alias = upstream_id[: -len("-free")]
        return alias or None
    return None


def main() -> None:
    with urllib.request.urlopen(UPSTREAM, timeout=20) as resp:
        payload = json.load(resp)
    ids = [m["id"] for m in payload.get("data", [])]
    free_ids = sorted(i for i in ids if is_free_candidate(i))

    dynamic_public: list[str] = []
    for upstream_id in free_ids:
        if upstream_id in RESERVED_UPSTREAM:
            continue
        alias = public_alias(upstream_id)
        if alias and alias not in STATIC_PUBLIC:
            dynamic_public.append(alias)

    all_public = sorted(set(STATIC_PUBLIC.keys()) | set(dynamic_public))

    print(f"upstream total models: {len(ids)}")
    print(f"upstream free-ish ids: {len(free_ids)}")
    for mid in free_ids:
        print(f"  - {mid}")
    print()
    print(f"zenproxy static public ({len(STATIC_PUBLIC)}):")
    for pub, up in STATIC_PUBLIC.items():
        print(f"  - {pub} -> {up}")
    print()
    print(f"zenproxy dynamic public with discovery+candidate_canary_or_active ({len(dynamic_public)}):")
    for pub in dynamic_public:
        print(f"  - {pub} -> {pub}-free")
    print()
    print(f"expected /v1/models total: {len(all_public)}")
    for pub in all_public:
        print(f"  - {pub}")


if __name__ == "__main__":
    main()
