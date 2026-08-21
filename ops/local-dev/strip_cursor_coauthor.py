#!/usr/bin/env python3
"""Rewrite git history to strip Co-authored-by: Cursor trailers from all commits."""
from __future__ import annotations

import os
import re
import subprocess
import sys

CURSOR_COAUTHOR = re.compile(
    r"^Co-authored-by:\s*Cursor\s*<cursoragent@cursor\.com>\s*\n?",
    re.I | re.M,
)
GENERIC_CURSOR = re.compile(
    r"^Co-authored-by:.*cursor.*\n?",
    re.I | re.M,
)
AUTHOR_RE = re.compile(r"^(.*?) <([^>]+)> (\d+ [+-]\d+)$")


def parse_identity(line: str) -> tuple[str, str, str]:
    m = AUTHOR_RE.match(line.strip())
    if not m:
        raise RuntimeError(f"cannot parse identity line: {line!r}")
    return m.group(1), m.group(2), m.group(3)


def strip_trailers(msg: bytes) -> bytes:
    text = msg.decode("utf-8", errors="replace")
    text = CURSOR_COAUTHOR.sub("", text)
    text = GENERIC_CURSOR.sub("", text)
    text = text.rstrip() + "\n"
    return text.encode("utf-8")


def rewrite_repo(repo: str, ref: str) -> tuple[str, int]:
    os.chdir(repo)
    commits = subprocess.check_output(
        ["git", "rev-list", ref], text=True
    ).splitlines()
    commits.reverse()  # oldest first

    mapping: dict[str, str] = {}
    rewritten = 0

    for commit in commits:
        body = subprocess.check_output(
            ["git", "cat-file", "-p", commit], text=True
        )
        lines = body.splitlines()
        tree = None
        parents: list[str] = []
        author_line = None
        committer_line = None
        in_message = False
        message_lines: list[str] = []

        for line in lines:
            if line.startswith("tree "):
                tree = line.split()[1]
            elif line.startswith("parent "):
                parents.append(line.split()[1])
            elif line.startswith("author "):
                author_line = line[len("author ") :]
            elif line.startswith("committer "):
                committer_line = line[len("committer ") :]
            elif line == "" and not in_message:
                in_message = True
            elif in_message:
                message_lines.append(line)

        if not tree or author_line is None or committer_line is None:
            raise RuntimeError(f"failed to parse commit {commit}")

        old_msg = "\n".join(message_lines).encode("utf-8") + b"\n"
        new_msg = strip_trailers(old_msg)
        new_parents = [mapping[p] for p in parents]
        parent_changed = any(np != op for np, op in zip(new_parents, parents))
        if new_msg == old_msg and not parent_changed:
            mapping[commit] = commit
            continue

        rewritten += 1
        author_name, author_email, author_date = parse_identity(author_line)
        committer_name, committer_email, committer_date = parse_identity(committer_line)
        env = os.environ.copy()
        env["GIT_AUTHOR_NAME"] = author_name
        env["GIT_AUTHOR_EMAIL"] = author_email
        env["GIT_AUTHOR_DATE"] = author_date
        env["GIT_COMMITTER_NAME"] = committer_name
        env["GIT_COMMITTER_EMAIL"] = committer_email
        env["GIT_COMMITTER_DATE"] = committer_date

        cmd = ["git", "commit-tree", tree]
        for p in new_parents:
            cmd.extend(["-p", p])
        new_commit = subprocess.check_output(cmd, input=new_msg, env=env, text=False).decode().strip()
        mapping[commit] = new_commit

    new_tip = mapping[commits[-1]]
    return new_tip, rewritten


def main() -> int:
    if len(sys.argv) != 3:
        print(f"usage: {sys.argv[0]} <repo> <ref>", file=sys.stderr)
        return 2

    repo, ref = sys.argv[1], sys.argv[2]
    new_tip, rewritten = rewrite_repo(repo, ref)
    print(f"repo={repo} ref={ref} rewritten={rewritten} new_tip={new_tip}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
