#!/usr/bin/env python3
"""Flush GitHub homepage sidebar contributor cache by toggling default branch."""
from __future__ import annotations

import json
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


def gh_input(method: str, path: str, payload: dict | None = None) -> None:
    cmd = [GH, "api", "-X", method, path]
    if payload is not None:
        cmd.extend(["--input", "-"])
        stdin = json.dumps(payload)
    else:
        stdin = None
    subprocess.run(cmd, input=stdin, text=True, check=True)


def ensure_temp_branch(main_sha: str) -> None:
    try:
        gh_input(
            "POST",
            f"repos/{REPO}/git/refs",
            {"ref": f"refs/heads/{TEMP_BRANCH}", "sha": main_sha},
        )
        print(f"created branch {TEMP_BRANCH}")
    except subprocess.CalledProcessError:
        gh_input(
            "PATCH",
            f"repos/{REPO}/git/refs/heads/{TEMP_BRANCH}",
            {"sha": main_sha, "force": True},
        )
        print(f"updated branch {TEMP_BRANCH}")


def run() -> int:
    default = gh_field(f"repos/{REPO}", ".default_branch")
    main_sha = gh_field(f"repos/{REPO}/git/ref/heads/{MAIN}", ".object.sha")
    print(f"current default={default} main={main_sha[:12]}")

    ensure_temp_branch(main_sha)

    if default != TEMP_BRANCH:
        gh_input("PATCH", f"repos/{REPO}", {"default_branch": TEMP_BRANCH})
        print(f"default -> {TEMP_BRANCH}")
        time.sleep(45)
    else:
        print("default already temp; will switch back to main")

    gh_input("PATCH", f"repos/{REPO}", {"default_branch": MAIN})
    print(f"default -> {MAIN}")
    time.sleep(45)

    try:
        subprocess.run(
            [GH, "api", "-X", "DELETE", f"repos/{REPO}/git/refs/heads/{TEMP_BRANCH}"],
            check=True,
        )
        print(f"deleted temp branch {TEMP_BRANCH}")
    except subprocess.CalledProcessError as exc:
        print(f"warn: delete temp branch failed: {exc}")

    final_default = gh_field(f"repos/{REPO}", ".default_branch")
    logins = gh("api", f"repos/{REPO}/contributors", "--jq", ".[].login")
    print("final default:", final_default)
    print("contributors api:", logins)
    return 0


if __name__ == "__main__":
    raise SystemExit(run())
