#!/usr/bin/env python3
import re
import subprocess
import sys

PAT = re.compile(r"co-authored-by:\s*cursor", re.I)


def scan(repo: str, ref: str) -> list[tuple[str, str]]:
    commits = subprocess.check_output(
        ["git", "log", ref, "--format=%H"], cwd=repo, text=True
    ).splitlines()
    hits: list[tuple[str, str]] = []
    for h in commits:
        body = subprocess.check_output(
            ["git", "log", "-1", "--format=%B", h], cwd=repo, text=True
        )
        if PAT.search(body):
            subj = subprocess.check_output(
                ["git", "log", "-1", "--format=%s", h], cwd=repo, text=True
            ).strip()
            hits.append((h, subj))
    return hits


def main() -> int:
    targets = [
        ("Zenproxyrs", "/home/lenovo/zen-free-model-suite/dist/zen-proxy-rs", "origin/main"),
        ("monorepo", "/home/lenovo/zen-free-model-suite", "github/main"),
    ]
    for name, repo, ref in targets:
        print(f"=== {name} ===")
        try:
            hits = scan(repo, ref)
            print(f"hits: {len(hits)}")
            for h, s in hits:
                print(f"  {h[:12]} {s}")
        except subprocess.CalledProcessError as e:
            print(f"ERROR: {e}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
