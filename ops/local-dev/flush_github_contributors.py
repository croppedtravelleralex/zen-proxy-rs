#!/usr/bin/env python3
"""Flush GitHub homepage sidebar contributor cache by toggling default branch."""
from __future__ import annotations

import subprocess
import sys
import time

GH = "/mnt/c/Program Files/GitHub CLI/gh.exe"
REPO = "croppedtravelleralex/Zenproxyrs"
TEMP_BRANCH = "chore/flush-contributors-cache"
MAIN = "main"


def gh(*args: str) -> str:
    return subprocess.check_output([GH, *args], text=True).strip()


def gh_field(path: str, field: str) -> str:
    return gh("api", path, "--jq", field).strip('"')


def gh_api(method: str, path: str, **fields: str) -> None:
    cmd = [GH, "api", "-X", method, path]
    for k, v in fields.items():
        cmd.extend(["-f", f"{k}={v}"])
    subprocess.check_call(cmd)


def run() -> int:
    default = gh_field(f"repos/{REPO}", ".default_branch")
    print(f"current default={default}")

    main_sha = gh_field(f"repos/{REPO}/git/ref/heads/{MAIN}", ".object.sha")
    print(f"main sha={main_sha[:12]}")
    try:
        gh_api("POST", f"repos/{REPO}/git/refs", ref=f"refs/heads/{TEMP_BRANCH}", sha=main_sha)
        print(f"created branch {TEMP_BRANCH}")
    except subprocess.CalledProcessError:
        gh_api("PATCH", f"repos/{REPO}/git/refs/heads/{TEMP_BRANCH}", sha=main_sha, force="true")
        print(f"updated branch {TEMP_BRANCH}")

    if default != TEMP_BRANCH:
        gh_api("PATCH", f"repos/{REPO}", default_branch=TEMP_BRANCH)
        print(f"default -> {TEMP_BRANCH}")
        time.sleep(45)

    gh_api("PATCH", f"repos/{REPO}", default_branch=MAIN)
    print(f"default -> {MAIN}")
    time.sleep(45)

    try:
        gh_api("DELETE", f"repos/{REPO}/git/refs/heads/{TEMP_BRANCH}")
        print(f"deleted temp branch {TEMP_BRANCH}")
    except subprocess.CalledProcessError as exc:
        print(f"warn: could not delete temp branch: {exc}")

    logins = gh("api", f"repos/{REPO}/contributors", "--jq", ".[].login")
    print("contributors api:", logins)
    return 0


if __name__ == "__main__":
    raise SystemExit(run())
