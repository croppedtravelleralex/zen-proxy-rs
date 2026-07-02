#!/usr/bin/env python3
"""
ClaudeCode acceptance runner for dynamic-model compatibility.

Default mode is diagnostic dry-run. Use --execute to run the real matrix.
Reports are redacted: they store command shape, status, timing, hashes, marker
checks, and ClaudeCode metadata only. They do not store API keys, prompts, full
completions, or tool outputs.
"""

from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import os
import platform
import re
import shlex
import shutil
import subprocess
import sys
import tempfile
import time
import uuid
from dataclasses import dataclass
from pathlib import Path
from typing import Any


DEFAULT_BASE_URL = "https://new.relai.asia"
PRODUCTION_BASE_HOST = "sub2api.closeapi.top"
DEFAULT_MODELS = ["mimo-v2.5", "north-mini-code", "nemotron-3-ultra"]
DEFAULT_HELPER_MODEL = "claude-haiku-4-5"
DEFAULT_WINDOWS_CLAUDE = r"C:\Users\Lenovo\.local\bin\claude.orig.exe"
DEFAULT_WSL_CLAUDE = "/home/lenovo/.local/bin/claude"
DEFAULT_WSL_DISTRO = "HermesUbuntu"
DEFAULT_WSL_USER = "lenovo"
DEFAULT_TIMEOUT = 300.0
SLOW_WEBFETCH_MS = 180_000
DEFAULT_SUITE = "full"

SECRET_PATTERNS = [
    re.compile(r"sk-[A-Za-z0-9_\-]{6,}"),
    re.compile(r"Bearer\s+[A-Za-z0-9._\-]+", re.IGNORECASE),
    re.compile(r"(api[_-]?key|authorization|token|secret|password)([\"'\s:=]+)([^\"'\s,}]+)", re.IGNORECASE),
]


@dataclass(frozen=True)
class WorkspaceCheck:
    path: str
    contains: str


@dataclass(frozen=True)
class ToolCaseSpec:
    base_case_id: str
    tool: str
    allowed_tool: str
    prompt: str
    expected_marker: str
    slow_after_ms: int | None = None
    workspace_checks: tuple[WorkspaceCheck, ...] = ()


@dataclass(frozen=True)
class AcceptanceCase:
    case_id: str
    base_case_id: str
    output_format: str
    tool: str
    allowed_tool: str
    prompt: str
    expected_marker: str
    slow_after_ms: int | None = None
    workspace_checks: tuple[WorkspaceCheck, ...] = ()


@dataclass(frozen=True)
class CommandResult:
    command: list[str]
    exit_code: int | None
    elapsed_ms: int
    stdout_bytes: int
    stderr_bytes: int
    stdout_sha256: str
    stderr_sha256: str
    stdout_text: str
    stderr_text: str
    error: str = ""


OUTPUT_FORMATS = ["text", "json", "stream-json"]

WORKSPACE_FILES = {
    "markers/bash-value.txt": "BASH_MARKER_ALPHA_01\n",
    "notes/read-marker.txt": "READ_MARKER_ALPHA_01\n",
    "notes/read-offset.txt": "line one\nOFFSET_MARKER_ALPHA_01\nline three\n",
    "notes/empty.txt": "",
    "notes/path with spaces.txt": "READ_SPACE_MARKER_ALPHA_01\n",
    "notes/long-read.txt": "\n".join(f"long line {index:03d}" for index in range(1, 121))
    + "\nREAD_LONG_MARKER_ALPHA_01\n",
    "notes/alpha.md": "# Alpha\n\nALPHA_MARKER_VALUE_01 lives here.\n",
    "data/table.csv": "name,value\ncsv-marker,CSV_MARKER_ALPHA_01\n",
    "src/sample.py": "def sample():\n    return 'PY_MARKER_ALPHA_01'\n",
    "src/extra.py": "EXTRA_MARKER_ALPHA_01 = True\n",
    "nested/deep/glob-target-GLOB_MARKER_ALPHA_01.txt": "glob target\n",
    "nested/deep/other.log": "not the target\n",
    "logs/large-output.txt": "\n".join(f"large filler line {index:03d}" for index in range(1, 301))
    + "\nBASH_LARGE_MARKER_ALPHA_01\n",
    "logs/grep.log": "INFO start\nGREP_TARGET=GREP_MARKER_ALPHA_01\nINFO end\n",
    "logs/regex.log": "request_id=REQ-CLAUDECODE-4242 status=ok\n",
    "logs/include.txt": "include marker INCLUDE_MARKER_ALPHA_01\n",
    "logs/multi.log": "MULTI_MATCH_ALPHA_01 first\nignore\nMULTI_MATCH_ALPHA_01 second\n",
    "edit-single.txt": "before=EDIT_BEFORE_ALPHA_01\n",
    "edit-all.txt": "color=red\nshape=red\n",
    "edit-insert.txt": "first\nthird\n",
    "edit-multiline.txt": "start\nold one\nold two\nend\n",
    "edit-missing.txt": "unchanged=EDIT_MISSING_ORIGINAL_ALPHA_01\n",
    "multiedit.txt": "alpha=old\nbeta=old\ngamma=old\n",
    "todo-source.txt": "TODO_MARKER_ALPHA_01\n",
    "existing/overwrite.txt": "OVERWRITE_OLD_ALPHA_01\n",
    "notebooks/sample.ipynb": json.dumps(
        {
            "cells": [
                {
                    "cell_type": "markdown",
                    "metadata": {},
                    "source": ["NOTEBOOK_MARKER_ALPHA_01\n"],
                },
                {
                    "cell_type": "code",
                    "execution_count": None,
                    "metadata": {},
                    "outputs": [],
                    "source": ["value = 'NOTEBOOK_CODE_ALPHA_01'\n"],
                },
            ],
            "metadata": {
                "kernelspec": {
                    "display_name": "Python 3",
                    "language": "python",
                    "name": "python3",
                },
                "language_info": {"name": "python", "pygments_lexer": "ipython3"},
            },
            "nbformat": 4,
            "nbformat_minor": 5,
        },
        indent=2,
    )
    + "\n",
}

TOOL_CASE_SPECS = [
    ToolCaseSpec(
        base_case_id="bash",
        tool="Bash",
        allowed_tool="Bash(cat markers/bash-value.txt)",
        prompt="Use the Bash tool exactly once to run `cat markers/bash-value.txt`. After the tool result, reply with exactly `BASH_OK:<marker-value>` and nothing else.",
        expected_marker="BASH_OK:BASH_MARKER_ALPHA_01",
    ),
    ToolCaseSpec(
        base_case_id="bash_stderr",
        tool="Bash",
        allowed_tool="Bash",
        prompt="Use the Bash tool exactly once to run `sh -c 'echo BASH_STDERR_MARKER_ALPHA_01 1>&2; echo BASH_STDOUT_DONE_ALPHA_01'`. After the tool result, reply with exactly `BASH_STDERR_OK:BASH_STDERR_MARKER_ALPHA_01` and nothing else.",
        expected_marker="BASH_STDERR_OK:BASH_STDERR_MARKER_ALPHA_01",
    ),
    ToolCaseSpec(
        base_case_id="bash_nonzero",
        tool="Bash",
        allowed_tool="Bash",
        prompt="Use the Bash tool exactly once to run `sh -c 'echo BASH_NONZERO_MARKER_ALPHA_01; exit 7'`. After the tool result, reply with exactly `BASH_NONZERO_OK:BASH_NONZERO_MARKER_ALPHA_01:7` and nothing else.",
        expected_marker="BASH_NONZERO_OK:BASH_NONZERO_MARKER_ALPHA_01:7",
    ),
    ToolCaseSpec(
        base_case_id="bash_large_stdout",
        tool="Bash",
        allowed_tool="Bash(cat logs/large-output.txt)",
        prompt="Use the Bash tool exactly once to run `cat logs/large-output.txt`. After the tool result, reply with exactly `BASH_LARGE_OK:BASH_LARGE_MARKER_ALPHA_01` and nothing else.",
        expected_marker="BASH_LARGE_OK:BASH_LARGE_MARKER_ALPHA_01",
    ),
    ToolCaseSpec(
        base_case_id="read_text",
        tool="Read",
        allowed_tool="Read",
        prompt="Use the Read tool exactly once on `notes/read-marker.txt`. The file contains a marker value. Reply with exactly `READ_OK:<marker-value>` and nothing else.",
        expected_marker="READ_OK:READ_MARKER_ALPHA_01",
    ),
    ToolCaseSpec(
        base_case_id="read_empty",
        tool="Read",
        allowed_tool="Read",
        prompt="Use the Read tool exactly once on `notes/empty.txt`. If the file is empty, reply with exactly `READ_EMPTY_OK:empty` and nothing else.",
        expected_marker="READ_EMPTY_OK:empty",
    ),
    ToolCaseSpec(
        base_case_id="read_long",
        tool="Read",
        allowed_tool="Read",
        prompt="Use the Read tool exactly once on `notes/long-read.txt`. Reply with exactly `READ_LONG_OK:READ_LONG_MARKER_ALPHA_01` and nothing else.",
        expected_marker="READ_LONG_OK:READ_LONG_MARKER_ALPHA_01",
    ),
    ToolCaseSpec(
        base_case_id="read_space_path",
        tool="Read",
        allowed_tool="Read",
        prompt="Use the Read tool exactly once on `notes/path with spaces.txt`. Reply with exactly `READ_SPACE_OK:READ_SPACE_MARKER_ALPHA_01` and nothing else.",
        expected_marker="READ_SPACE_OK:READ_SPACE_MARKER_ALPHA_01",
    ),
    ToolCaseSpec(
        base_case_id="read_offset",
        tool="Read",
        allowed_tool="Read",
        prompt="Use the Read tool exactly once on `notes/read-offset.txt`. Reply with exactly `READ_OFFSET_OK:<marker-value-on-second-line>` and nothing else.",
        expected_marker="READ_OFFSET_OK:OFFSET_MARKER_ALPHA_01",
    ),
    ToolCaseSpec(
        base_case_id="read_csv",
        tool="Read",
        allowed_tool="Read",
        prompt="Use the Read tool exactly once on `data/table.csv`. Reply with exactly `READ_CSV_OK:<csv-marker-value>` and nothing else.",
        expected_marker="READ_CSV_OK:CSV_MARKER_ALPHA_01",
    ),
    ToolCaseSpec(
        base_case_id="ls_root",
        tool="LS",
        allowed_tool="LS",
        prompt="Use the LS tool exactly once on the current directory. Reply with exactly `LS_ROOT_OK:notes` if the `notes` directory is present, and nothing else.",
        expected_marker="LS_ROOT_OK:notes",
    ),
    ToolCaseSpec(
        base_case_id="ls_nested",
        tool="LS",
        allowed_tool="LS",
        prompt="Use the LS tool exactly once on `nested/deep`. Find the glob target filename and reply with exactly `LS_NESTED_OK:<filename>` and nothing else.",
        expected_marker="LS_NESTED_OK:glob-target-GLOB_MARKER_ALPHA_01.txt",
    ),
    ToolCaseSpec(
        base_case_id="glob_txt",
        tool="Glob",
        allowed_tool="Glob",
        prompt="Use the Glob tool exactly once with pattern `**/glob-target-*.txt`. Reply with exactly `GLOB_TXT_OK:<matched-basename>` and nothing else.",
        expected_marker="GLOB_TXT_OK:glob-target-GLOB_MARKER_ALPHA_01.txt",
    ),
    ToolCaseSpec(
        base_case_id="glob_py",
        tool="Glob",
        allowed_tool="Glob",
        prompt="Use the Glob tool exactly once with pattern `src/sample.py`. Reply with exactly `GLOB_PY_OK:<matched-basename>` and nothing else.",
        expected_marker="GLOB_PY_OK:sample.py",
    ),
    ToolCaseSpec(
        base_case_id="glob_markdown",
        tool="Glob",
        allowed_tool="Glob",
        prompt="Use the Glob tool exactly once with pattern `notes/*.md`. Reply with exactly `GLOB_MD_OK:alpha.md` and nothing else.",
        expected_marker="GLOB_MD_OK:alpha.md",
    ),
    ToolCaseSpec(
        base_case_id="grep_plain",
        tool="Grep",
        allowed_tool="Grep",
        prompt="Use the Grep tool exactly once with pattern `GREP_TARGET`, path `logs`, and output_mode `content`. Reply with exactly `GREP_PLAIN_OK:<value after GREP_TARGET=>` and nothing else.",
        expected_marker="GREP_PLAIN_OK:GREP_MARKER_ALPHA_01",
    ),
    ToolCaseSpec(
        base_case_id="grep_regex",
        tool="Grep",
        allowed_tool="Grep",
        prompt="Use the Grep tool exactly once with pattern `request_id=REQ-[A-Z-]+-[0-9]+`, path `logs/regex.log`, and output_mode `content`. Reply with exactly `GREP_REGEX_OK:<request-id>` and nothing else.",
        expected_marker="GREP_REGEX_OK:REQ-CLAUDECODE-4242",
    ),
    ToolCaseSpec(
        base_case_id="grep_include",
        tool="Grep",
        allowed_tool="Grep",
        prompt="Use the Grep tool exactly once with pattern `INCLUDE_MARKER_[A-Z]+_[0-9]+`, path `logs`, glob `include.txt`, and output_mode `content`. Reply with exactly `GREP_INCLUDE_OK:<marker>` and nothing else.",
        expected_marker="GREP_INCLUDE_OK:INCLUDE_MARKER_ALPHA_01",
    ),
    ToolCaseSpec(
        base_case_id="grep_no_match",
        tool="Grep",
        allowed_tool="Grep",
        prompt="Use the Grep tool exactly once with pattern `NO_SUCH_MARKER_ALPHA_01`, path `logs`, and output_mode `content`. If there are no matches, reply with exactly `GREP_NO_MATCH_OK:0` and nothing else.",
        expected_marker="GREP_NO_MATCH_OK:0",
    ),
    ToolCaseSpec(
        base_case_id="grep_multi_match",
        tool="Grep",
        allowed_tool="Grep",
        prompt="Use the Grep tool exactly once with pattern `MULTI_MATCH_ALPHA_01`, path `logs/multi.log`, and output_mode `content`. Count the matching lines and reply with exactly `GREP_MULTI_OK:2` and nothing else.",
        expected_marker="GREP_MULTI_OK:2",
    ),
    ToolCaseSpec(
        base_case_id="write_file",
        tool="Write",
        allowed_tool="Write",
        prompt="Use the Write tool exactly once to create `created/write-file.txt` containing exactly `WRITE_MARKER_ALPHA_01`. After the tool result, reply with exactly `WRITE_FILE_OK` and nothing else.",
        expected_marker="WRITE_FILE_OK",
        workspace_checks=(WorkspaceCheck("created/write-file.txt", "WRITE_MARKER_ALPHA_01"),),
    ),
    ToolCaseSpec(
        base_case_id="write_json",
        tool="Write",
        allowed_tool="Write",
        prompt='Use the Write tool exactly once to create `created/payload.json` containing exactly `{"marker":"WRITE_JSON_MARKER_ALPHA_01"}`. After the tool result, reply with exactly `WRITE_JSON_OK` and nothing else.',
        expected_marker="WRITE_JSON_OK",
        workspace_checks=(WorkspaceCheck("created/payload.json", "WRITE_JSON_MARKER_ALPHA_01"),),
    ),
    ToolCaseSpec(
        base_case_id="write_nested",
        tool="Write",
        allowed_tool="Write",
        prompt="Use the Write tool exactly once to create `created/nested/deep.txt` containing exactly `WRITE_NESTED_MARKER_ALPHA_01`. After the tool result, reply with exactly `WRITE_NESTED_OK` and nothing else.",
        expected_marker="WRITE_NESTED_OK",
        workspace_checks=(WorkspaceCheck("created/nested/deep.txt", "WRITE_NESTED_MARKER_ALPHA_01"),),
    ),
    ToolCaseSpec(
        base_case_id="write_overwrite",
        tool="Read,Write",
        allowed_tool="Read,Write",
        prompt="Use the Read tool exactly once on `existing/overwrite.txt`, then use the Write tool exactly once to overwrite it with exactly `OVERWRITE_NEW_ALPHA_01`. After the Write tool result, reply with exactly `WRITE_OVERWRITE_OK` and nothing else.",
        expected_marker="WRITE_OVERWRITE_OK",
        workspace_checks=(WorkspaceCheck("existing/overwrite.txt", "OVERWRITE_NEW_ALPHA_01"),),
    ),
    ToolCaseSpec(
        base_case_id="edit_single",
        tool="Read,Edit",
        allowed_tool="Read,Edit",
        prompt="Use the Read tool exactly once on `edit-single.txt`, then use the Edit tool exactly once to replace `EDIT_BEFORE_ALPHA_01` with `EDIT_AFTER_ALPHA_01`. After the Edit tool result, reply with exactly `EDIT_SINGLE_OK` and nothing else.",
        expected_marker="EDIT_SINGLE_OK",
        workspace_checks=(WorkspaceCheck("edit-single.txt", "EDIT_AFTER_ALPHA_01"),),
    ),
    ToolCaseSpec(
        base_case_id="edit_replace_all",
        tool="Read,Edit",
        allowed_tool="Read,Edit",
        prompt="Use the Read tool exactly once on `edit-all.txt`, then use the Edit tool exactly once with replace_all set to true to replace every `red` value with `blue`. After the Edit tool result, reply with exactly `EDIT_ALL_OK` and nothing else.",
        expected_marker="EDIT_ALL_OK",
        workspace_checks=(WorkspaceCheck("edit-all.txt", "color=blue"), WorkspaceCheck("edit-all.txt", "shape=blue")),
    ),
    ToolCaseSpec(
        base_case_id="edit_insert",
        tool="Read,Edit",
        allowed_tool="Read,Edit",
        prompt="Use the Read tool exactly once on `edit-insert.txt`, then use the Edit tool exactly once to insert a line containing `second=EDIT_INSERT_ALPHA_01` between `first` and `third`. After the Edit tool result, reply with exactly `EDIT_INSERT_OK` and nothing else.",
        expected_marker="EDIT_INSERT_OK",
        workspace_checks=(WorkspaceCheck("edit-insert.txt", "second=EDIT_INSERT_ALPHA_01"),),
    ),
    ToolCaseSpec(
        base_case_id="edit_multiline",
        tool="Read,Edit",
        allowed_tool="Read,Edit",
        prompt="Use the Read tool exactly once on `edit-multiline.txt`, then use the Edit tool exactly once to replace the two-line block `old one` followed by `old two` with the two-line block `new one` followed by `new two`. After the Edit tool result, reply with exactly `EDIT_MULTILINE_OK` and nothing else.",
        expected_marker="EDIT_MULTILINE_OK",
        workspace_checks=(WorkspaceCheck("edit-multiline.txt", "new one\nnew two"),),
    ),
    ToolCaseSpec(
        base_case_id="edit_missing_old",
        tool="Read,Edit",
        allowed_tool="Read,Edit",
        prompt="Use the Read tool exactly once on `edit-missing.txt`, then use the Edit tool exactly once attempting to replace `NOT_PRESENT_ALPHA_01` with `SHOULD_NOT_APPEAR_ALPHA_01`. After the Edit tool result, reply with exactly `EDIT_MISSING_OK:not_found` and nothing else.",
        expected_marker="EDIT_MISSING_OK:not_found",
        workspace_checks=(WorkspaceCheck("edit-missing.txt", "EDIT_MISSING_ORIGINAL_ALPHA_01"),),
    ),
    ToolCaseSpec(
        base_case_id="multiedit_two",
        tool="Read,MultiEdit",
        allowed_tool="Read,MultiEdit",
        prompt="Use the Read tool exactly once on `multiedit.txt`, then use the MultiEdit tool exactly once to replace `alpha=old` with `alpha=MULTI_ALPHA_01` and `beta=old` with `beta=MULTI_BETA_01`. After the MultiEdit tool result, reply with exactly `MULTIEDIT_TWO_OK` and nothing else.",
        expected_marker="MULTIEDIT_TWO_OK",
        workspace_checks=(WorkspaceCheck("multiedit.txt", "alpha=MULTI_ALPHA_01"), WorkspaceCheck("multiedit.txt", "beta=MULTI_BETA_01")),
    ),
    ToolCaseSpec(
        base_case_id="multiedit_three",
        tool="Read,MultiEdit",
        allowed_tool="Read,MultiEdit",
        prompt="Use the Read tool exactly once on `multiedit.txt`, then use the MultiEdit tool exactly once to replace all three old values with `MULTI_A_02`, `MULTI_B_02`, and `MULTI_G_02`. After the MultiEdit tool result, reply with exactly `MULTIEDIT_THREE_OK` and nothing else.",
        expected_marker="MULTIEDIT_THREE_OK",
        workspace_checks=(WorkspaceCheck("multiedit.txt", "MULTI_A_02"), WorkspaceCheck("multiedit.txt", "MULTI_B_02"), WorkspaceCheck("multiedit.txt", "MULTI_G_02")),
    ),
    ToolCaseSpec(
        base_case_id="todowrite_single",
        tool="TodoWrite",
        allowed_tool="TodoWrite",
        prompt="Use the TodoWrite tool exactly once to create one todo item with content `TODO_MARKER_ALPHA_01` and status `in_progress`. After the tool result, reply with exactly `TODOWRITE_SINGLE_OK` and nothing else.",
        expected_marker="TODOWRITE_SINGLE_OK",
    ),
    ToolCaseSpec(
        base_case_id="todowrite_multiple",
        tool="TodoWrite",
        allowed_tool="TodoWrite",
        prompt="Use the TodoWrite tool exactly once to create two todo items containing `TODO_FIRST_ALPHA_01` and `TODO_SECOND_ALPHA_01`. After the tool result, reply with exactly `TODOWRITE_MULTI_OK` and nothing else.",
        expected_marker="TODOWRITE_MULTI_OK",
    ),
    ToolCaseSpec(
        base_case_id="notebook_read",
        tool="NotebookRead",
        allowed_tool="NotebookRead",
        prompt="Use the NotebookRead tool exactly once on `notebooks/sample.ipynb`. Reply with exactly `NOTEBOOK_READ_OK:<markdown-marker>` and nothing else.",
        expected_marker="NOTEBOOK_READ_OK:NOTEBOOK_MARKER_ALPHA_01",
    ),
    ToolCaseSpec(
        base_case_id="notebook_edit",
        tool="NotebookRead,NotebookEdit",
        allowed_tool="NotebookRead,NotebookEdit",
        prompt="Use the NotebookRead tool exactly once on `notebooks/sample.ipynb`, then use the NotebookEdit tool exactly once to replace `NOTEBOOK_CODE_ALPHA_01` with `NOTEBOOK_EDITED_ALPHA_01`. After the NotebookEdit tool result, reply with exactly `NOTEBOOK_EDIT_OK` and nothing else.",
        expected_marker="NOTEBOOK_EDIT_OK",
        workspace_checks=(WorkspaceCheck("notebooks/sample.ipynb", "NOTEBOOK_EDITED_ALPHA_01"),),
    ),
    ToolCaseSpec(
        base_case_id="webfetch",
        tool="WebFetch",
        allowed_tool="WebFetch",
        prompt="Use WebFetch exactly once on https://example.com/. After the tool result, reply with exactly `WEBFETCH_OK` and nothing else.",
        expected_marker="WEBFETCH_OK",
        slow_after_ms=SLOW_WEBFETCH_MS,
    ),
    ToolCaseSpec(
        base_case_id="webfetch_iana",
        tool="WebFetch",
        allowed_tool="WebFetch",
        prompt="Use WebFetch exactly once on https://www.iana.org/domains/reserved. After the tool result, reply with exactly `WEBFETCH_IANA_OK` and nothing else.",
        expected_marker="WEBFETCH_IANA_OK",
        slow_after_ms=SLOW_WEBFETCH_MS,
    ),
    ToolCaseSpec(
        base_case_id="webfetch_404",
        tool="WebFetch",
        allowed_tool="WebFetch",
        prompt="Use WebFetch exactly once on https://example.com/claudecode-acceptance-missing-page. After the tool result, reply with exactly `WEBFETCH_404_OK` and nothing else.",
        expected_marker="WEBFETCH_404_OK",
        slow_after_ms=SLOW_WEBFETCH_MS,
    ),
    ToolCaseSpec(
        base_case_id="websearch",
        tool="WebSearch",
        allowed_tool="WebSearch",
        prompt="Use WebSearch exactly once to search `OpenAI official site`. After the tool result, reply with exactly `WEBSEARCH_OK` and nothing else.",
        expected_marker="WEBSEARCH_OK",
    ),
    ToolCaseSpec(
        base_case_id="websearch_claudecode",
        tool="WebSearch",
        allowed_tool="WebSearch",
        prompt="Use WebSearch exactly once to search `Claude Code official docs`. After the tool result, reply with exactly `WEBSEARCH_CLAUDECODE_OK` and nothing else.",
        expected_marker="WEBSEARCH_CLAUDECODE_OK",
    ),
    ToolCaseSpec(
        base_case_id="websearch_chinese",
        tool="WebSearch",
        allowed_tool="WebSearch",
        prompt="Use WebSearch exactly once to search `Claude Code 官方 文档`. After the tool result, reply with exactly `WEBSEARCH_CHINESE_OK` and nothing else.",
        expected_marker="WEBSEARCH_CHINESE_OK",
    ),
    ToolCaseSpec(
        base_case_id="task_simple",
        tool="Task",
        allowed_tool="Task",
        prompt="Use the Task tool exactly once to ask a general-purpose subagent to return `TASK_MARKER_ALPHA_01`. After the tool result, reply with exactly `TASK_SIMPLE_OK:TASK_MARKER_ALPHA_01` and nothing else.",
        expected_marker="TASK_SIMPLE_OK:TASK_MARKER_ALPHA_01",
        slow_after_ms=120_000,
    ),
    ToolCaseSpec(
        base_case_id="task_json_marker",
        tool="Task",
        allowed_tool="Task",
        prompt='Use the Task tool exactly once to ask a general-purpose subagent to return exactly `{"marker":"TASK_JSON_MARKER_ALPHA_01"}`. After the tool result, reply with exactly `TASK_JSON_OK:TASK_JSON_MARKER_ALPHA_01` and nothing else.',
        expected_marker="TASK_JSON_OK:TASK_JSON_MARKER_ALPHA_01",
        slow_after_ms=120_000,
    ),
]

SMOKE_CASE_IDS = ["bash", "webfetch", "websearch"]
DEFAULT_EXCLUDED_CASE_REASONS = {
    "ls_root": "ClaudeCode 2.1.143 does not register any tool schema for --tools LS/Ls/ls in print mode.",
    "ls_nested": "ClaudeCode 2.1.143 does not register any tool schema for --tools LS/Ls/ls in print mode.",
    "multiedit_two": "ClaudeCode 2.1.143 print mode does not register MultiEdit; use Edit coverage instead.",
    "multiedit_three": "ClaudeCode 2.1.143 print mode does not register MultiEdit; use Edit coverage instead.",
    "todowrite_single": "ClaudeCode 2.1.143 print mode does not register TodoWrite.",
    "todowrite_multiple": "ClaudeCode 2.1.143 print mode does not register TodoWrite.",
    "notebook_read": "ClaudeCode 2.1.143 print mode does not register NotebookRead.",
    "notebook_edit": "ClaudeCode 2.1.143 print mode cannot satisfy NotebookEdit's read-before-edit prerequisite because NotebookRead is unavailable.",
}
P1_EXTENDED_CASE_IDS = [
    "bash_stderr",
    "bash_nonzero",
    "bash_large_stdout",
    "read_empty",
    "read_long",
    "read_space_path",
    "grep_no_match",
    "grep_multi_match",
    "write_overwrite",
    "edit_multiline",
    "edit_missing_old",
    "webfetch_404",
    "websearch_chinese",
    "task_json_marker",
]
CORE_CASE_IDS = [
    spec.base_case_id
    for spec in TOOL_CASE_SPECS
    if spec.tool not in {"Task", "WebFetch", "WebSearch"}
    and spec.base_case_id not in DEFAULT_EXCLUDED_CASE_REASONS
    and spec.base_case_id not in P1_EXTENDED_CASE_IDS
]
SUITE_CASE_IDS = {
    "smoke": SMOKE_CASE_IDS,
    "core": CORE_CASE_IDS,
    "full": [
        spec.base_case_id
        for spec in TOOL_CASE_SPECS
        if spec.base_case_id not in DEFAULT_EXCLUDED_CASE_REASONS and spec.base_case_id not in P1_EXTENDED_CASE_IDS
    ],
}
EXTENDED_CASE_IDS = SUITE_CASE_IDS["full"] + P1_EXTENDED_CASE_IDS
SUITE_CASE_IDS["extended"] = EXTENDED_CASE_IDS


def output_format_suffix(output_format: str) -> str:
    return output_format.replace("-", "_")


def expand_case(spec: ToolCaseSpec, output_format: str) -> AcceptanceCase:
    return AcceptanceCase(
        case_id=f"{spec.base_case_id}_{output_format_suffix(output_format)}",
        base_case_id=spec.base_case_id,
        output_format=output_format,
        tool=spec.tool,
        allowed_tool=spec.allowed_tool,
        prompt=spec.prompt,
        expected_marker=spec.expected_marker,
        slow_after_ms=spec.slow_after_ms,
        workspace_checks=spec.workspace_checks,
    )


CASE_SPEC_BY_ID = {case.base_case_id: case for case in TOOL_CASE_SPECS}
CASES = [expand_case(spec, output_format) for spec in TOOL_CASE_SPECS for output_format in OUTPUT_FORMATS]
CASE_BY_ID = {case.case_id: case for case in CASES}


def utc_run_id() -> str:
    return dt.datetime.now(dt.UTC).strftime("%Y%m%d-%H%M%S") + "-claudecode-acceptance"


def is_wsl() -> bool:
    try:
        return "microsoft" in Path("/proc/version").read_text(encoding="utf-8", errors="replace").lower()
    except Exception:
        return False


def is_windows_host() -> bool:
    return os.name == "nt"


def redact(text: str, extra_values: list[str] | None = None) -> str:
    out = text
    for value in extra_values or []:
        if value:
            out = out.replace(value, "[REDACTED]")
    for pattern in SECRET_PATTERNS:
        if "api" in pattern.pattern.lower() or "authorization" in pattern.pattern.lower():
            out = pattern.sub(r"\1\2[REDACTED]", out)
        else:
            out = pattern.sub("[REDACTED]", out)
    return out


def sha256_text(text: str) -> str:
    return hashlib.sha256(text.encode("utf-8", errors="replace")).hexdigest()


def write_workspace_fixture(workspace: Path) -> None:
    for relative_path, content in WORKSPACE_FILES.items():
        target = workspace / relative_path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(content, encoding="utf-8")


def workspace_check_results(case: AcceptanceCase, workspace: Path | None) -> tuple[bool | None, list[dict[str, Any]]]:
    if not case.workspace_checks:
        return None, []
    if workspace is None:
        return False, [{"path": check.path, "contains": check.contains, "ok": False, "error": "workspace unavailable"} for check in case.workspace_checks]
    results: list[dict[str, Any]] = []
    for check in case.workspace_checks:
        target = workspace / check.path
        try:
            content = target.read_text(encoding="utf-8", errors="replace")
            ok = check.contains in content
            error = ""
        except Exception as exc:
            ok = False
            error = f"{type(exc).__name__}:{exc}"
        results.append({"path": check.path, "contains": check.contains, "ok": ok, "error": error})
    return all(item["ok"] for item in results), results


def shell_quote(args: list[str], secret_values: list[str], prompt_values: list[str]) -> str:
    redacted: list[str] = []
    for part in args:
        if part in prompt_values:
            redacted.append("[REDACTED_PROMPT]")
        elif any(secret and part == secret for secret in secret_values):
            redacted.append("[REDACTED]")
        else:
            redacted.append(part)
    return " ".join(shlex.quote(part) for part in redacted)


def windows_path_for_current_host(path: str) -> str:
    if is_windows_host():
        return path
    match = re.match(r"^([A-Za-z]):\\(.*)$", path)
    if match and is_wsl():
        drive = match.group(1).lower()
        rest = match.group(2).replace("\\", "/")
        return f"/mnt/{drive}/{rest}"
    return path


def windows_path_for_powershell(path: str) -> str:
    match = re.match(r"^/mnt/([A-Za-z])/(.*)$", path)
    if match:
        drive = match.group(1).upper()
        rest = match.group(2).replace("/", "\\")
        return f"{drive}:\\{rest}"
    return path


def command_exists(path_or_name: str) -> str | None:
    candidate = windows_path_for_current_host(path_or_name)
    if "/" in candidate or "\\" in candidate:
        return candidate if Path(candidate).exists() else None
    return shutil.which(candidate)


def build_claude_command(
    binary: str,
    model: str,
    case: AcceptanceCase,
    add_dir: str = ".",
    permission_mode: str = "acceptEdits",
) -> list[str]:
    command = [
        binary,
        "-p",
        case.prompt,
        "--model",
        model,
        "--output-format",
        case.output_format,
        "--no-session-persistence",
        "--add-dir",
        add_dir,
        "--permission-mode",
        permission_mode,
        "--tools",
        case.tool,
        "--allowedTools",
        case.allowed_tool,
    ]
    if case.output_format == "stream-json":
        command.append("--verbose")
    return command


def run_command(
    args: list[str],
    timeout: float,
    api_key: str,
    env: dict[str, str],
    cwd: Path | str | None = None,
) -> CommandResult:
    started = time.perf_counter()
    try:
        proc = subprocess.run(
            args,
            stdin=subprocess.DEVNULL,
            text=True,
            encoding="utf-8",
            errors="replace",
            capture_output=True,
            timeout=timeout,
            env=env,
            cwd=str(cwd) if cwd else None,
            check=False,
        )
        elapsed_ms = int((time.perf_counter() - started) * 1000)
        stdout = redact(proc.stdout or "", [api_key])
        stderr = redact(proc.stderr or "", [api_key])
        return CommandResult(
            command=args,
            exit_code=proc.returncode,
            elapsed_ms=elapsed_ms,
            stdout_bytes=len(stdout.encode("utf-8", errors="replace")),
            stderr_bytes=len(stderr.encode("utf-8", errors="replace")),
            stdout_sha256=sha256_text(stdout),
            stderr_sha256=sha256_text(stderr),
            stdout_text=stdout,
            stderr_text=stderr,
        )
    except subprocess.TimeoutExpired as exc:
        elapsed_ms = int((time.perf_counter() - started) * 1000)
        stdout = redact(exc.stdout or "", [api_key])
        stderr = redact(exc.stderr or "", [api_key])
        return CommandResult(
            command=args,
            exit_code=None,
            elapsed_ms=elapsed_ms,
            stdout_bytes=len(stdout.encode("utf-8", errors="replace")),
            stderr_bytes=len(stderr.encode("utf-8", errors="replace")),
            stdout_sha256=sha256_text(stdout),
            stderr_sha256=sha256_text(stderr),
            stdout_text=stdout,
            stderr_text=stderr,
            error=f"TimeoutExpired:{timeout}s",
        )
    except Exception as exc:
        elapsed_ms = int((time.perf_counter() - started) * 1000)
        return CommandResult(
            command=args,
            exit_code=None,
            elapsed_ms=elapsed_ms,
            stdout_bytes=0,
            stderr_bytes=0,
            stdout_sha256=sha256_text(""),
            stderr_sha256=sha256_text(""),
            stdout_text="",
            stderr_text="",
            error=f"{type(exc).__name__}:{exc}",
        )


def run_powershell_bridge(
    display_command: list[str],
    timeout: float,
    api_key: str,
    base_url: str,
    model: str,
    helper_model: str,
    cwd_windows: str | None = None,
) -> CommandResult:
    def ps_quote(value: str) -> str:
        return "'" + value.replace("'", "''") + "'"

    binary = windows_path_for_powershell(display_command[0])
    arg_lines = []
    for part in display_command[1:]:
        arg_lines.append("$claudeArgs += " + ps_quote(part))
    script = "\n".join(
        [
            "$ErrorActionPreference = 'Stop'",
            "$env:ANTHROPIC_API_KEY = " + ps_quote(api_key),
            "$env:ANTHROPIC_AUTH_TOKEN = $env:ANTHROPIC_API_KEY",
            "$env:ANTHROPIC_BASE_URL = " + ps_quote(base_url.rstrip("/")),
            "$env:ANTHROPIC_MODEL = " + ps_quote(model),
            "$env:ANTHROPIC_SMALL_FAST_MODEL = " + ps_quote(helper_model),
            f"$env:API_TIMEOUT_MS = '{int(timeout * 1000)}'",
            "$env:CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC = '1'",
            "$env:DISABLE_INSTALLATION_CHECKS = '1'",
            "$env:DISABLE_TELEMETRY = '1'",
            "$env:USE_BUILTIN_RIPGREP = '0'",
            "$claude = " + ps_quote(binary),
            "$claudeArgs = @()",
            *([f"Set-Location -LiteralPath {ps_quote(cwd_windows)}"] if cwd_windows else []),
            *arg_lines,
            "& $claude @claudeArgs",
            "exit $LASTEXITCODE",
        ]
    )
    started = time.perf_counter()
    try:
        proc = subprocess.run(
            ["powershell.exe", "-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", "-"],
            input=script,
            text=True,
            encoding="utf-8",
            errors="replace",
            capture_output=True,
            timeout=timeout,
            check=False,
        )
        elapsed_ms = int((time.perf_counter() - started) * 1000)
        stdout = redact(proc.stdout or "", [api_key, script])
        stderr = redact(proc.stderr or "", [api_key, script])
        return CommandResult(
            command=display_command,
            exit_code=proc.returncode,
            elapsed_ms=elapsed_ms,
            stdout_bytes=len(stdout.encode("utf-8", errors="replace")),
            stderr_bytes=len(stderr.encode("utf-8", errors="replace")),
            stdout_sha256=sha256_text(stdout),
            stderr_sha256=sha256_text(stderr),
            stdout_text=stdout,
            stderr_text=stderr,
        )
    except subprocess.TimeoutExpired as exc:
        elapsed_ms = int((time.perf_counter() - started) * 1000)
        stdout = redact(exc.stdout or "", [api_key, script])
        stderr = redact(exc.stderr or "", [api_key, script])
        return CommandResult(
            command=display_command,
            exit_code=None,
            elapsed_ms=elapsed_ms,
            stdout_bytes=len(stdout.encode("utf-8", errors="replace")),
            stderr_bytes=len(stderr.encode("utf-8", errors="replace")),
            stdout_sha256=sha256_text(stdout),
            stderr_sha256=sha256_text(stderr),
            stdout_text=stdout,
            stderr_text=stderr,
            error=f"TimeoutExpired:{timeout}s",
        )
    except Exception as exc:
        elapsed_ms = int((time.perf_counter() - started) * 1000)
        return CommandResult(
            command=display_command,
            exit_code=None,
            elapsed_ms=elapsed_ms,
            stdout_bytes=0,
            stderr_bytes=0,
            stdout_sha256=sha256_text(""),
            stderr_sha256=sha256_text(""),
            stdout_text="",
            stderr_text="",
            error=f"{type(exc).__name__}:{exc}",
        )


def claude_env(base_url: str, api_key: str, model: str, helper_model: str, timeout: float) -> dict[str, str]:
    env = os.environ.copy()
    env.update(
        {
            "ANTHROPIC_API_KEY": api_key,
            "ANTHROPIC_AUTH_TOKEN": api_key,
            "ANTHROPIC_BASE_URL": base_url.rstrip("/"),
            "ANTHROPIC_MODEL": model,
            "ANTHROPIC_SMALL_FAST_MODEL": helper_model,
            "API_TIMEOUT_MS": str(int(timeout * 1000)),
            "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC": "1",
            "DISABLE_INSTALLATION_CHECKS": "1",
            "DISABLE_TELEMETRY": "1",
            "USE_BUILTIN_RIPGREP": "0",
        }
    )
    return env


def parse_server_tool_use(usage: dict[str, Any]) -> dict[str, int]:
    server_tool_use = usage.get("server_tool_use") if isinstance(usage.get("server_tool_use"), dict) else {}
    return {
        "web_fetch_requests": int(server_tool_use.get("web_fetch_requests") or 0),
        "web_search_requests": int(server_tool_use.get("web_search_requests") or 0),
    }


def parse_claude_json(stdout: str) -> dict[str, Any]:
    try:
        parsed = json.loads(stdout)
    except Exception:
        return {"json_parse_ok": False}
    if not isinstance(parsed, dict):
        return {"json_parse_ok": False}
    result_text = str(parsed.get("result") or "")
    usage = parsed.get("usage") if isinstance(parsed.get("usage"), dict) else {}
    server_tool_counts = parse_server_tool_use(usage)
    return {
        "json_parse_ok": True,
        "output_parse_kind": "json",
        "type": parsed.get("type"),
        "subtype": parsed.get("subtype"),
        "is_error": parsed.get("is_error"),
        "api_error_status": parsed.get("api_error_status"),
        "num_turns": parsed.get("num_turns"),
        "duration_ms": parsed.get("duration_ms"),
        "duration_api_ms": parsed.get("duration_api_ms"),
        "web_fetch_requests": server_tool_counts["web_fetch_requests"],
        "web_search_requests": server_tool_counts["web_search_requests"],
        "result_text": result_text,
        "stream_event_count": None,
    }


def parse_claude_stream_json(stdout: str) -> dict[str, Any]:
    events: list[dict[str, Any]] = []
    for line in stdout.splitlines():
        if not line.strip():
            continue
        try:
            item = json.loads(line)
        except Exception:
            return {"json_parse_ok": False, "output_parse_kind": "stream-json", "stream_event_count": len(events)}
        if isinstance(item, dict):
            events.append(item)
    result_event = next((item for item in reversed(events) if item.get("type") == "result"), None)
    if not result_event:
        return {"json_parse_ok": bool(events), "output_parse_kind": "stream-json", "stream_event_count": len(events)}
    usage = result_event.get("usage") if isinstance(result_event.get("usage"), dict) else {}
    server_tool_counts = parse_server_tool_use(usage)
    return {
        "json_parse_ok": True,
        "output_parse_kind": "stream-json",
        "type": result_event.get("type"),
        "subtype": result_event.get("subtype"),
        "is_error": result_event.get("is_error"),
        "api_error_status": result_event.get("api_error_status"),
        "num_turns": result_event.get("num_turns"),
        "duration_ms": result_event.get("duration_ms"),
        "duration_api_ms": result_event.get("duration_api_ms"),
        "web_fetch_requests": server_tool_counts["web_fetch_requests"],
        "web_search_requests": server_tool_counts["web_search_requests"],
        "result_text": str(result_event.get("result") or ""),
        "stream_event_count": len(events),
    }


def parse_claude_output(stdout: str, output_format: str) -> dict[str, Any]:
    if output_format == "json":
        return parse_claude_json(stdout)
    if output_format == "stream-json":
        return parse_claude_stream_json(stdout)
    return {
        "json_parse_ok": None,
        "output_parse_kind": "text",
        "type": "text",
        "subtype": None,
        "is_error": False,
        "api_error_status": None,
        "num_turns": None,
        "duration_ms": None,
        "duration_api_ms": None,
        "web_fetch_requests": 0,
        "web_search_requests": 0,
        "result_text": stdout,
        "stream_event_count": None,
    }


def inferred_tool_call_count(case: AcceptanceCase, parsed: dict[str, Any], marker_seen: bool) -> int | None:
    turns = parsed.get("num_turns")
    if isinstance(turns, int) and turns >= 2:
        return 1
    if case.output_format == "text" and marker_seen:
        return 1
    if int(parsed.get("web_search_requests") or 0) >= 1:
        return int(parsed.get("web_search_requests") or 0)
    return None


def tool_execution_ok(case: AcceptanceCase, parsed: dict[str, Any], marker_seen: bool) -> bool:
    if case.output_format == "text":
        return marker_seen
    if int(parsed.get("num_turns") or 0) >= 2:
        return True
    if case.tool == "WebSearch" and int(parsed.get("web_search_requests") or 0) >= 1:
        return True
    return False


def failure_kind(result: CommandResult, parsed: dict[str, Any], marker_seen: bool, tool_seen: bool) -> str:
    if result.exit_code == 0 and not result.error and marker_seen and tool_seen and not parsed.get("is_error"):
        return ""
    if result.error.startswith("TimeoutExpired"):
        if result.stdout_bytes == 0 and result.stderr_bytes == 0:
            return "timeout_no_output"
        return "timeout_partial_output"
    if parsed.get("is_error"):
        status = parsed.get("api_error_status")
        return f"claude_api_error_{status}" if status else "claude_api_error"
    if result.exit_code not in (0, None):
        return "claude_exit_nonzero"
    if not marker_seen:
        return "marker_missing"
    if not tool_seen:
        return "tool_execution_not_seen"
    return "unknown_failure"


def result_to_record(
    platform_name: str,
    model: str,
    case: AcceptanceCase,
    result: CommandResult,
    api_key: str,
    prompt_values: list[str],
    include_diagnostics: bool,
    workspace: Path | None = None,
) -> dict[str, Any]:
    parsed = parse_claude_output(result.stdout_text, case.output_format)
    result_text = str(parsed.get("result_text") or result.stdout_text)
    marker_seen = case.expected_marker in result_text
    workspace_checks_ok, workspace_checks = workspace_check_results(case, workspace)
    tool_seen = tool_execution_ok(case, parsed, marker_seen)
    parsed_error = bool(parsed.get("is_error"))
    kind = failure_kind(result, parsed, marker_seen, tool_seen)
    if workspace_checks_ok is False and not kind:
        kind = "workspace_check_failed"
    passed = (
        result.exit_code == 0
        and not result.error
        and marker_seen
        and tool_seen
        and not parsed_error
        and workspace_checks_ok is not False
    )
    slow = bool(case.slow_after_ms and result.elapsed_ms > case.slow_after_ms and passed)
    record: dict[str, Any] = {
        "platform": platform_name,
        "model": model,
        "case_id": case.case_id,
        "base_case_id": case.base_case_id,
        "output_format": case.output_format,
        "tool": case.tool,
        "command": shell_quote(result.command, [api_key], prompt_values),
        "exit_code": result.exit_code,
        "elapsed_ms": result.elapsed_ms,
        "status": "slow_pass" if slow else "pass" if passed else "fail",
        "failure_kind": "" if passed else kind,
        "timeout_no_output": kind == "timeout_no_output",
        "slow_threshold_ms": case.slow_after_ms,
        "expected_marker": case.expected_marker,
        "expected_marker_seen": marker_seen,
        "tool_execution_seen": tool_seen,
        "tool_call_count_inferred": inferred_tool_call_count(case, parsed, marker_seen),
        "workspace_checks_ok": workspace_checks_ok,
        "workspace_checks": workspace_checks,
        "stdout_bytes": result.stdout_bytes,
        "stderr_bytes": result.stderr_bytes,
        "stdout_sha256": result.stdout_sha256,
        "stderr_sha256": result.stderr_sha256,
        "error": result.error,
        "claude_json_parse_ok": parsed.get("json_parse_ok", False),
        "claude_output_parse_kind": parsed.get("output_parse_kind"),
        "claude_type": parsed.get("type"),
        "claude_subtype": parsed.get("subtype"),
        "claude_is_error": parsed.get("is_error"),
        "claude_api_error_status": parsed.get("api_error_status"),
        "claude_num_turns": parsed.get("num_turns"),
        "claude_duration_ms": parsed.get("duration_ms"),
        "claude_duration_api_ms": parsed.get("duration_api_ms"),
        "claude_web_fetch_requests": parsed.get("web_fetch_requests"),
        "claude_web_search_requests": parsed.get("web_search_requests"),
        "claude_stream_event_count": parsed.get("stream_event_count"),
    }
    if include_diagnostics:
        record["stdout_preview"] = redact(result.stdout_text[:800], [api_key])
        record["stderr_preview"] = redact(result.stderr_text[:800], [api_key])
    return record


def run_windows_case(
    binary: str,
    model: str,
    helper_model: str,
    case: AcceptanceCase,
    args: argparse.Namespace,
    api_key: str,
) -> dict[str, Any]:
    command = build_claude_command(binary, model, case, permission_mode=args.permission_mode)
    if is_wsl() and not is_windows_host() and not args.allow_wsl_windows_bridge:
        return {
            "platform": "windows",
            "model": model,
            "case_id": case.case_id,
            "tool": case.tool,
            "command": shell_quote(command, [api_key], [case.prompt]),
            "exit_code": None,
            "elapsed_ms": 0,
            "status": "skipped",
            "slow_threshold_ms": case.slow_after_ms,
            "expected_marker": case.expected_marker,
            "expected_marker_seen": False,
            "stdout_bytes": 0,
            "stderr_bytes": 0,
            "stdout_sha256": sha256_text(""),
            "stderr_sha256": sha256_text(""),
            "error": "WindowsExecutionRequiresWindowsHost",
            "claude_json_parse_ok": False,
            "claude_type": None,
            "claude_subtype": None,
            "claude_is_error": None,
            "claude_num_turns": None,
            "claude_duration_ms": None,
            "claude_duration_api_ms": None,
        }
    workspace_path: Path | None = None
    if is_wsl() and not is_windows_host():
        bridge_temp_root = Path("/mnt/c/Users/Lenovo/AppData/Local/Temp")
        with tempfile.TemporaryDirectory(
            prefix="claudecode-acceptance-bridge-workspace-",
            dir=str(bridge_temp_root) if bridge_temp_root.exists() else None,
        ) as workspace:
            workspace_path = Path(workspace)
            write_workspace_fixture(workspace_path)
            windows_workspace = windows_path_for_powershell(str(workspace_path))
            command = build_claude_command(binary, model, case, windows_workspace, args.permission_mode)
            result = run_powershell_bridge(
                command,
                args.timeout,
                api_key,
                args.base_url,
                model,
                helper_model,
                cwd_windows=windows_workspace,
            )
    else:
        env = claude_env(args.base_url, api_key, model, helper_model, args.timeout)
        with tempfile.TemporaryDirectory(prefix="claudecode-acceptance-winhome-") as temp_home, tempfile.TemporaryDirectory(
            prefix="claudecode-acceptance-workspace-"
        ) as workspace:
            workspace_path = Path(workspace)
            write_workspace_fixture(workspace_path)
            command = build_claude_command(binary, model, case, str(workspace_path), args.permission_mode)
            temp_home_path = Path(temp_home)
            roaming = temp_home_path / "AppData" / "Roaming"
            local = temp_home_path / "AppData" / "Local"
            roaming.mkdir(parents=True, exist_ok=True)
            local.mkdir(parents=True, exist_ok=True)
            env["USERPROFILE"] = temp_home
            env["HOME"] = temp_home
            env["APPDATA"] = str(roaming)
            env["LOCALAPPDATA"] = str(local)
            result = run_command(command, args.timeout, api_key, env, cwd=workspace_path)
            return result_to_record(
                "windows",
                model,
                case,
                result,
                api_key,
                [case.prompt],
                args.include_diagnostics,
                workspace=workspace_path,
            )
    return result_to_record("windows", model, case, result, api_key, [case.prompt], args.include_diagnostics, workspace=workspace_path)


def wsl_env_with_key(base_env: dict[str, str], api_key: str, extra: dict[str, str] | None = None) -> dict[str, str]:
    env = base_env.copy()
    env["ANTHROPIC_API_KEY"] = api_key
    for key, value in (extra or {}).items():
        env[key] = value
    names = [item for item in env.get("WSLENV", "").split(":") if item]
    for key in ["ANTHROPIC_API_KEY", *(extra or {}).keys()]:
        entry = f"{key}/u"
        if entry not in names:
            names.append(entry)
    env["WSLENV"] = ":".join(names)
    return env


def run_wsl_case_from_windows(
    model: str,
    helper_model: str,
    case: AcceptanceCase,
    args: argparse.Namespace,
    api_key: str,
) -> dict[str, Any]:
    bash_payload = r"""
	set -euo pipefail
	tmp_home="$CLAUDECODE_ACCEPTANCE_TEMP_HOME"
	tmp_workspace="$CLAUDECODE_ACCEPTANCE_TEMP_WORKSPACE"
	[ -n "$tmp_home" ]
	[ -n "$tmp_workspace" ]
	mkdir -m 700 "$tmp_home"
	mkdir -m 700 "$tmp_workspace"
	cleanup() { rm -rf "$tmp_home" "$tmp_workspace"; }
	trap cleanup EXIT
	mkdir -p "$tmp_home/.clawgod"
	export HOME="$tmp_home"
	python3 - <<'PY'
	import json
	import os
	import sys
	path = os.path.join(os.environ["HOME"], ".clawgod", "provider.json")
with open(path, "w", encoding="utf-8") as handle:
    json.dump(
        {
            "apiKey": os.environ["ANTHROPIC_API_KEY"],
            "baseURL": os.environ["CLAUDECODE_ACCEPTANCE_BASE_URL"],
            "model": os.environ["CLAUDECODE_ACCEPTANCE_MODEL"],
            "smallModel": os.environ["CLAUDECODE_ACCEPTANCE_HELPER_MODEL"],
            "timeoutMs": 300000,
        },
        handle,
        indent=2,
	    )
	    handle.write("\n")
	workspace = os.environ["CLAUDECODE_ACCEPTANCE_TEMP_WORKSPACE"]
	for relative_path, content in json.loads(os.environ["CLAUDECODE_ACCEPTANCE_WORKSPACE_FILES_JSON"]).items():
	    target = os.path.join(workspace, relative_path)
	    os.makedirs(os.path.dirname(target), exist_ok=True)
	    with open(target, "w", encoding="utf-8") as fixture:
	        fixture.write(content)
	PY
	cd "$tmp_workspace"
	export CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1
	export DISABLE_INSTALLATION_CHECKS=1
	export DISABLE_TELEMETRY=1
	export USE_BUILTIN_RIPGREP=0
verbose_arg=()
if [ "$CLAUDECODE_ACCEPTANCE_OUTPUT_FORMAT" = "stream-json" ]; then
  verbose_arg=(--verbose)
fi
exec "$CLAUDECODE_ACCEPTANCE_WSL_BIN" \
  -p "$CLAUDECODE_ACCEPTANCE_PROMPT" \
  --model "$CLAUDECODE_ACCEPTANCE_MODEL" \
	  --output-format "$CLAUDECODE_ACCEPTANCE_OUTPUT_FORMAT" \
	  --no-session-persistence \
	  --add-dir "$tmp_workspace" \
	  --permission-mode "$CLAUDECODE_ACCEPTANCE_PERMISSION_MODE" \
  --tools "$CLAUDECODE_ACCEPTANCE_TOOL" \
  --allowedTools "$CLAUDECODE_ACCEPTANCE_ALLOWED_TOOL" \
  "${verbose_arg[@]}" \
  </dev/null
"""
    command = [
        "wsl.exe",
        "-d",
        args.wsl_distro,
        "-u",
        args.wsl_user,
        "--",
        "bash",
        "-lc",
        bash_payload,
    ]
    env = wsl_env_with_key(
        os.environ.copy(),
        api_key,
        {
            "CLAUDECODE_ACCEPTANCE_BASE_URL": args.base_url.rstrip("/"),
            "CLAUDECODE_ACCEPTANCE_MODEL": model,
            "CLAUDECODE_ACCEPTANCE_HELPER_MODEL": helper_model,
            "CLAUDECODE_ACCEPTANCE_WSL_BIN": args.wsl_claude_bin,
            "CLAUDECODE_ACCEPTANCE_PROMPT": case.prompt,
            "CLAUDECODE_ACCEPTANCE_TOOL": case.tool,
            "CLAUDECODE_ACCEPTANCE_ALLOWED_TOOL": case.allowed_tool,
            "CLAUDECODE_ACCEPTANCE_OUTPUT_FORMAT": case.output_format,
            "CLAUDECODE_ACCEPTANCE_PERMISSION_MODE": args.permission_mode,
            "CLAUDECODE_ACCEPTANCE_TEMP_HOME": f"/tmp/claudecode-acceptance-home-{uuid.uuid4().hex}",
            "CLAUDECODE_ACCEPTANCE_TEMP_WORKSPACE": f"/tmp/claudecode-acceptance-workspace-{uuid.uuid4().hex}",
            "CLAUDECODE_ACCEPTANCE_WORKSPACE_FILES_JSON": json.dumps(WORKSPACE_FILES, ensure_ascii=False),
        },
    )
    result = run_command(command, args.timeout + 20.0, api_key, env)
    display = build_claude_command(args.wsl_claude_bin, model, case, permission_mode=args.permission_mode)
    result = CommandResult(
        command=display,
        exit_code=result.exit_code,
        elapsed_ms=result.elapsed_ms,
        stdout_bytes=result.stdout_bytes,
        stderr_bytes=result.stderr_bytes,
        stdout_sha256=result.stdout_sha256,
        stderr_sha256=result.stderr_sha256,
        stdout_text=result.stdout_text,
        stderr_text=result.stderr_text,
        error=result.error,
    )
    return result_to_record("wsl", model, case, result, api_key, [case.prompt], args.include_diagnostics)


def run_wsl_case_local(
    model: str,
    helper_model: str,
    case: AcceptanceCase,
    args: argparse.Namespace,
    api_key: str,
) -> dict[str, Any]:
    with tempfile.TemporaryDirectory(prefix="claudecode-acceptance-home-") as temp_home, tempfile.TemporaryDirectory(
        prefix="claudecode-acceptance-workspace-"
    ) as workspace:
        workspace_path = Path(workspace)
        write_workspace_fixture(workspace_path)
        provider_dir = Path(temp_home) / ".clawgod"
        provider_dir.mkdir(parents=True, exist_ok=True)
        provider_path = provider_dir / "provider.json"
        provider_path.write_text(
            json.dumps(
                {
                    "apiKey": api_key,
                    "baseURL": args.base_url.rstrip("/"),
                    "model": model,
                    "smallModel": helper_model,
                    "timeoutMs": int(args.timeout * 1000),
                },
                indent=2,
            )
            + "\n",
            encoding="utf-8",
        )
        command = build_claude_command(args.wsl_claude_bin, model, case, str(workspace_path), args.permission_mode)
        env = claude_env(args.base_url, api_key, model, helper_model, args.timeout)
        env["HOME"] = temp_home
        result = run_command(command, args.timeout, api_key, env, cwd=workspace_path)
        return result_to_record(
            "wsl",
            model,
            case,
            result,
            api_key,
            [case.prompt],
            args.include_diagnostics,
            workspace=workspace_path,
        )


def run_wsl_case(
    model: str,
    helper_model: str,
    case: AcceptanceCase,
    args: argparse.Namespace,
    api_key: str,
) -> dict[str, Any]:
    if is_windows_host():
        if not args.allow_windows_wsl_bridge:
            display = build_claude_command(args.wsl_claude_bin, model, case, permission_mode=args.permission_mode)
            return {
                "platform": "wsl",
                "model": model,
                "case_id": case.case_id,
                "tool": case.tool,
                "command": shell_quote(display, [api_key], [case.prompt]),
                "exit_code": None,
                "elapsed_ms": 0,
                "status": "skipped",
                "slow_threshold_ms": case.slow_after_ms,
                "expected_marker": case.expected_marker,
                "expected_marker_seen": False,
                "tool_execution_seen": False,
                "stdout_bytes": 0,
                "stderr_bytes": 0,
                "stdout_sha256": sha256_text(""),
                "stderr_sha256": sha256_text(""),
                "error": "WslExecutionRequiresWslHost",
                "claude_json_parse_ok": False,
                "claude_type": None,
                "claude_subtype": None,
                "claude_is_error": None,
                "claude_num_turns": None,
                "claude_duration_ms": None,
                "claude_duration_api_ms": None,
                "claude_web_fetch_requests": None,
                "claude_web_search_requests": None,
            }
        return run_wsl_case_from_windows(model, helper_model, case, args, api_key)
    return run_wsl_case_local(model, helper_model, case, args, api_key)


def selected_case_specs(suite: str, case_ids: list[str] | None) -> list[ToolCaseSpec]:
    if case_ids:
        unknown = [case_id for case_id in case_ids if case_id not in CASE_SPEC_BY_ID and case_id not in CASE_BY_ID]
        if unknown:
            raise SystemExit(f"unknown --cases value(s): {', '.join(unknown)}")
        specs = [CASE_SPEC_BY_ID[case_id] for case_id in case_ids if case_id in CASE_SPEC_BY_ID]
        exact_specs = [CASE_SPEC_BY_ID[CASE_BY_ID[case_id].base_case_id] for case_id in case_ids if case_id in CASE_BY_ID]
        seen: set[str] = set()
        out: list[ToolCaseSpec] = []
        for spec in [*specs, *exact_specs]:
            if spec.base_case_id not in seen:
                out.append(spec)
                seen.add(spec.base_case_id)
        return out
    return [CASE_SPEC_BY_ID[case_id] for case_id in SUITE_CASE_IDS[suite]]


def selected_cases(args: argparse.Namespace) -> list[AcceptanceCase]:
    selected: list[AcceptanceCase] = []
    exact_ids: set[str] = set()
    if args.cases:
        unknown = [case_id for case_id in args.cases if case_id not in CASE_SPEC_BY_ID and case_id not in CASE_BY_ID]
        if unknown:
            raise SystemExit(f"unknown --cases value(s): {', '.join(unknown)}")
        for case_id in args.cases:
            if case_id in CASE_BY_ID:
                case = CASE_BY_ID[case_id]
                if case.output_format in args.output_formats and case.case_id not in exact_ids:
                    selected.append(case)
                    exact_ids.add(case.case_id)
                continue
            spec = CASE_SPEC_BY_ID[case_id]
            for output_format in args.output_formats:
                case = expand_case(spec, output_format)
                if case.case_id not in exact_ids:
                    selected.append(case)
                    exact_ids.add(case.case_id)
        return selected
    for spec in selected_case_specs(args.suite, None):
        for output_format in args.output_formats:
            selected.append(expand_case(spec, output_format))
    return selected


def planned_commands(args: argparse.Namespace, windows_bin: str | None) -> list[dict[str, str]]:
    rows: list[dict[str, str]] = []
    platforms = selected_platforms(args.platform)
    cases = selected_cases(args)
    for model in args.models:
        for case in cases:
            if "windows" in platforms:
                binary = windows_bin or windows_path_for_current_host(args.windows_claude_bin)
                rows.append(
                    {
                        "platform": "windows",
                        "model": model,
                        "case": case.case_id,
                        "tool": case.tool,
                        "output_format": case.output_format,
                        "command": shell_quote(build_claude_command(binary, model, case, permission_mode=args.permission_mode), [], [case.prompt]),
                    }
                )
            if "wsl" in platforms:
                rows.append(
                    {
                        "platform": "wsl",
                        "model": model,
                        "case": case.case_id,
                        "tool": case.tool,
                        "output_format": case.output_format,
                        "command": shell_quote(build_claude_command(args.wsl_claude_bin, model, case, permission_mode=args.permission_mode), [], [case.prompt]),
                    }
                )
    return rows


def selected_platforms(value: str) -> list[str]:
    if value == "both":
        return ["windows", "wsl"]
    return [value]


def production_base_url(base_url: str) -> bool:
    return PRODUCTION_BASE_HOST in base_url.lower()


def discover(args: argparse.Namespace) -> dict[str, Any]:
    windows_candidate = windows_path_for_current_host(args.windows_claude_bin)
    return {
        "host_platform": platform.platform(),
        "python": sys.version.split()[0],
        "running_on_windows": is_windows_host(),
        "running_on_wsl": is_wsl(),
        "windows_claude_bin": command_exists(args.windows_claude_bin),
        "windows_claude_candidate": windows_candidate,
        "wsl_claude_bin": args.wsl_claude_bin if Path(args.wsl_claude_bin).exists() or is_windows_host() else None,
        "wsl_distro": args.wsl_distro,
        "wsl_user": args.wsl_user,
    }


def markdown_report(
    args: argparse.Namespace,
    run_id: str,
    discovery: dict[str, Any],
    planned: list[dict[str, str]],
    results: list[dict[str, Any]],
    diagnostics: list[str],
    api_key_present: bool,
) -> str:
    mode = "execute" if args.execute else "dry-run"
    selected = selected_cases(args)
    case_label = ", ".join(args.cases) if args.cases else f"{args.suite} suite"
    lines = [
        "# ClaudeCode Dynamic Model Acceptance",
        "",
        f"- run_id: `{run_id}`",
        f"- mode: `{mode}`",
        f"- base_url: `{args.base_url}`",
        f"- platforms: `{args.platform}`",
        f"- models: `{', '.join(args.models)}`",
        f"- suite: `{args.suite}`",
        f"- cases: `{case_label}`",
        f"- output_formats: `{', '.join(args.output_formats)}`",
        f"- permission_mode: `{args.permission_mode}`",
        f"- selected_case_count: `{len(selected)}`",
        f"- planned_matrix_items: `{len(planned)}`",
        f"- helper_model: `{args.helper_model}`",
        f"- timeout_secs: `{args.timeout}`",
        f"- post_case_delay_secs: `{args.post_case_delay}`",
        "- api_key: `[REDACTED]`" if api_key_present else f"- api_key_env: `{args.api_key_env}` missing",
        "",
        "## Discovery",
        "",
    ]
    for key, value in discovery.items():
        lines.append(f"- {key}: `{value}`")
    lines.extend(["", "## Planned Matrix", ""])
    for row in planned:
        lines.append(
            f"- `{row['platform']}` `{row['model']}` `{row['case']}` `{row['tool']}` `{row['output_format']}`: `{row['command']}`"
        )
    if not planned:
        lines.append("- none")
    lines.extend(["", "## Results", ""])
    if results:
        lines.append("| platform | model | case | tool | format | status | failure_kind | elapsed_ms | turns | tool_calls | marker | workspace | stdout_bytes | stderr_bytes |")
        lines.append("|---|---|---|---|---|---:|---|---:|---:|---:|---:|---:|---:|---:|")
        for item in results:
            lines.append(
                "| {platform} | {model} | {case_id} | {tool} | {output_format} | {status} | {failure_kind} | {elapsed_ms} | {turns} | {tool_calls} | {marker} | {workspace} | {stdout} | {stderr} |".format(
                    platform=item.get("platform"),
                    model=item.get("model"),
                    case_id=item.get("case_id"),
                    tool=item.get("tool", ""),
                    output_format=item.get("output_format", ""),
                    status=item.get("status"),
                    failure_kind=item.get("failure_kind", ""),
                    elapsed_ms=item.get("elapsed_ms"),
                    turns=item.get("claude_num_turns") if item.get("claude_num_turns") is not None else "",
                    tool_calls=item.get("tool_call_count_inferred") if item.get("tool_call_count_inferred") is not None else "",
                    marker="yes" if item.get("expected_marker_seen") else "no",
                    workspace="" if item.get("workspace_checks_ok") is None else "yes" if item.get("workspace_checks_ok") else "no",
                    stdout=item.get("stdout_bytes"),
                    stderr=item.get("stderr_bytes"),
                )
            )
        lines.extend(["", "### Tool Execution", ""])
        for item in results:
            lines.append(
                f"- `{item['platform']}/{item['model']}/{item['case_id']}` format=`{item.get('output_format')}` tool=`{item.get('tool')}` tool_execution_seen=`{item.get('tool_execution_seen')}` tool_call_count_inferred=`{item.get('tool_call_count_inferred')}` web_fetch_requests=`{item.get('claude_web_fetch_requests')}` web_search_requests=`{item.get('claude_web_search_requests')}` stream_events=`{item.get('claude_stream_event_count')}` workspace_checks_ok=`{item.get('workspace_checks_ok')}`"
            )
        lines.extend(["", "### Result Hashes", ""])
        for item in results:
            lines.append(
                f"- `{item['platform']}/{item['model']}/{item['case_id']}` stdout_sha256=`{item['stdout_sha256']}` stderr_sha256=`{item['stderr_sha256']}` exit=`{item['exit_code']}` error=`{item['error'] or ''}`"
            )
            if args.include_diagnostics and item.get("stderr_preview"):
                lines.append("  - stderr_preview: `[redacted; omitted from default reports]`")
    else:
        lines.append("- not executed")
    lines.extend(["", "## Diagnostics / Next Steps", ""])
    if diagnostics:
        lines.extend(f"- {item}" for item in diagnostics)
    else:
        lines.append("- No blocking diagnostics.")
    lines.append("")
    return "\n".join(lines)


def write_run_artifacts(
    args: argparse.Namespace,
    run_id: str,
    discovery: dict[str, Any],
    planned: list[dict[str, str]],
    results: list[dict[str, Any]],
    diagnostics: list[str],
    api_key_present: bool,
) -> tuple[Path, Path]:
    run_dir = Path(args.out_dir) / run_id
    run_dir.mkdir(parents=True, exist_ok=True)
    report_path = run_dir / "claudecode-acceptance.md"
    json_path = run_dir / "claudecode-acceptance.json"
    report_path.write_text(
        markdown_report(args, run_id, discovery, planned, results, diagnostics, api_key_present),
        encoding="utf-8",
    )
    json_path.write_text(
        json.dumps(
            {
                "run_id": run_id,
                "mode": "execute" if args.execute else "dry-run",
                "base_url": args.base_url,
                "platform": args.platform,
                "models": args.models,
                "suite": args.suite,
                "output_formats": args.output_formats,
                "permission_mode": args.permission_mode,
                "post_case_delay": args.post_case_delay,
                "cases": args.cases,
                "selected_case_count": len(selected_cases(args)),
                "helper_model": args.helper_model,
                "api_key_present": api_key_present,
                "discovery": discovery,
                "planned": planned,
                "results": results,
                "diagnostics": diagnostics,
            },
            ensure_ascii=False,
            indent=2,
        )
        + "\n",
        encoding="utf-8",
    )
    return report_path, json_path


def main() -> int:
    parser = argparse.ArgumentParser(description="Dry-run or execute ClaudeCode model/tool acceptance.")
    parser.add_argument("--execute", action="store_true", help="Run real ClaudeCode acceptance. Default is dry-run.")
    parser.add_argument("--run-id", default=os.getenv("CLAUDECODE_ACCEPTANCE_RUN_ID") or utc_run_id())
    parser.add_argument("--out-dir", default="test-records/runs")
    parser.add_argument("--base-url", default=os.getenv("ANTHROPIC_BASE_URL", DEFAULT_BASE_URL))
    parser.add_argument("--api-key-env", default="ANTHROPIC_API_KEY")
    parser.add_argument("--platform", choices=["windows", "wsl", "both"], default="both")
    parser.add_argument("--models", nargs="+", default=DEFAULT_MODELS)
    parser.add_argument("--suite", choices=sorted(SUITE_CASE_IDS), default=DEFAULT_SUITE)
    parser.add_argument("--output-formats", nargs="+", choices=OUTPUT_FORMATS, default=list(OUTPUT_FORMATS))
    parser.add_argument("--cases", nargs="+", default=None, help="Base case ids or expanded case ids. Omit to use --suite.")
    parser.add_argument(
        "--permission-mode",
        choices=["acceptEdits", "auto", "bypassPermissions", "default", "dontAsk", "plan"],
        default="acceptEdits",
    )
    parser.add_argument("--helper-model", default=os.getenv("CLAUDECODE_HELPER_MODEL", DEFAULT_HELPER_MODEL))
    parser.add_argument("--windows-claude-bin", default=os.getenv("WINDOWS_CLAUDE_BIN", DEFAULT_WINDOWS_CLAUDE))
    parser.add_argument("--wsl-claude-bin", default=os.getenv("WSL_CLAUDE_BIN", DEFAULT_WSL_CLAUDE))
    parser.add_argument("--wsl-distro", default=os.getenv("WSL_DISTRO", DEFAULT_WSL_DISTRO))
    parser.add_argument("--wsl-user", default=os.getenv("WSL_USER", DEFAULT_WSL_USER))
    parser.add_argument("--timeout", type=float, default=float(os.getenv("CLAUDECODE_ACCEPTANCE_TIMEOUT", str(DEFAULT_TIMEOUT))))
    parser.add_argument(
        "--post-case-delay",
        type=float,
        default=float(os.getenv("CLAUDECODE_ACCEPTANCE_POST_CASE_DELAY", "0")),
        help="Sleep this many seconds after each executed matrix item. Useful for rate-limited dev channels.",
    )
    parser.add_argument("--allow-production-base-url", action="store_true", help="Required to execute against production-like sub2api base URL.")
    parser.add_argument("--allow-wsl-windows-bridge", action="store_true", help="Allow experimental WSL->PowerShell execution for Windows ClaudeCode cases.")
    parser.add_argument("--allow-windows-wsl-bridge", action="store_true", help="Allow experimental Windows->WSL execution for WSL ClaudeCode cases.")
    parser.add_argument("--include-diagnostics", action="store_true", help="Include redacted stderr previews in JSON results.")
    args = parser.parse_args()

    api_key = os.getenv(args.api_key_env, "")
    diagnostics: list[str] = []
    results: list[dict[str, Any]] = []
    selected = selected_cases(args)
    discovery = discover(args)
    windows_bin = discovery.get("windows_claude_bin")
    planned = planned_commands(args, str(windows_bin) if windows_bin else None)

    if not args.cases and DEFAULT_EXCLUDED_CASE_REASONS:
        excluded = ", ".join(f"{case_id} ({reason})" for case_id, reason in DEFAULT_EXCLUDED_CASE_REASONS.items())
        diagnostics.append(f"Default suites exclude unsupported diagnostic case(s): {excluded}")
    if not api_key:
        diagnostics.append(f"{args.api_key_env} is missing; execute mode cannot call ClaudeCode.")
    if args.execute and production_base_url(args.base_url) and not args.allow_production_base_url:
        diagnostics.append(f"Refusing to execute against production-looking base URL containing {PRODUCTION_BASE_HOST}.")
    if args.execute and "windows" in selected_platforms(args.platform) and not windows_bin:
        diagnostics.append("Windows ClaudeCode executable was not found; Windows platform cases will fail if executed.")
    if args.execute and "windows" in selected_platforms(args.platform) and is_wsl() and not is_windows_host() and not args.allow_wsl_windows_bridge:
        diagnostics.append(
            "Windows platform execution is skipped from WSL by default; run this script under Windows Python for Windows ClaudeCode, or pass --allow-wsl-windows-bridge for the experimental bridge."
        )
    if args.execute and "wsl" in selected_platforms(args.platform) and not (is_windows_host() or Path(args.wsl_claude_bin).exists()):
        diagnostics.append("WSL ClaudeCode launcher was not found; WSL platform cases will fail if executed.")
    if args.execute and "wsl" in selected_platforms(args.platform) and is_windows_host() and not args.allow_windows_wsl_bridge:
        diagnostics.append(
            "WSL platform execution is skipped from Windows by default; run this script under WSL for WSL ClaudeCode, or pass --allow-windows-wsl-bridge for the experimental bridge."
        )

    should_execute = args.execute and api_key and not (
        production_base_url(args.base_url) and not args.allow_production_base_url
    )
    if should_execute:
        total_items = len(args.models) * len(selected) * len(selected_platforms(args.platform))
        executed_items = 0
        for model in args.models:
            for case in selected:
                if "windows" in selected_platforms(args.platform):
                    if windows_bin:
                        results.append(run_windows_case(str(windows_bin), model, args.helper_model, case, args, api_key))
                    else:
                        results.append(
                            {
                                "platform": "windows",
                                "model": model,
                                "case_id": case.case_id,
                                "tool": case.tool,
                                "status": "fail",
                                "failure_kind": "windows_claude_not_found",
                                "exit_code": None,
                                "elapsed_ms": 0,
                                "expected_marker": case.expected_marker,
                                "expected_marker_seen": False,
                                "stdout_bytes": 0,
                                "stderr_bytes": 0,
                                "stdout_sha256": sha256_text(""),
                                "stderr_sha256": sha256_text(""),
                                "error": "WindowsClaudeNotFound",
                            }
                        )
                    executed_items += 1
                    write_run_artifacts(args, args.run_id, discovery, planned, results, diagnostics, bool(api_key))
                    print(f"progress {executed_items}/{total_items} windows {model} {case.case_id} {results[-1].get('status')}", flush=True)
                    if args.post_case_delay > 0 and executed_items < total_items:
                        time.sleep(args.post_case_delay)
                if "wsl" in selected_platforms(args.platform):
                    results.append(run_wsl_case(model, args.helper_model, case, args, api_key))
                    executed_items += 1
                    write_run_artifacts(args, args.run_id, discovery, planned, results, diagnostics, bool(api_key))
                    print(f"progress {executed_items}/{total_items} wsl {model} {case.case_id} {results[-1].get('status')}", flush=True)
                    if args.post_case_delay > 0 and executed_items < total_items:
                        time.sleep(args.post_case_delay)

    report_path, _json_path = write_run_artifacts(args, args.run_id, discovery, planned, results, diagnostics, bool(api_key))
    print(str(report_path))

    if args.execute:
        failed = [item for item in results if item.get("status") in {"fail", "skipped"}]
        return 1 if failed or not should_execute else 0
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
