#!/usr/bin/env python3
"""
WSL-local OpenClaw/Hermes acceptance runner.

Default mode is diagnostic dry-run. Use --execute to run the safe P0 probes.
The report intentionally stores command shape, exit/status, timing, byte counts,
and hashes only. It never writes API keys or full model responses.
"""

from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import time
import urllib.error
import urllib.request
from dataclasses import dataclass
from pathlib import Path
from typing import Any


DEFAULT_BASE_URL = "http://127.0.0.1:8081"
DEFAULT_MODEL = "deepseek-v4-flash"
DEFAULT_HERMES_BIN = "/home/lenovo/.local/bin/hermes"
DEFAULT_OPENCLAW_BIN = "/home/lenovo/.local/node_modules/.bin/openclaw"
DEFAULT_OPENCLAW_CONFIG = "/home/lenovo/.openclaw-zenproxy-v46/openclaw.json"
USER_NODE22_BIN_DIR = "/home/lenovo/.local/node_modules/.bin"

SECRET_PATTERNS = [
    re.compile(r"sk-[A-Za-z0-9_\-]{6,}"),
    re.compile(r"Bearer\s+[A-Za-z0-9._\-]+", re.IGNORECASE),
    re.compile(r"(api[_-]?key|authorization|token|secret|password)([\"'\s:=]+)([^\"'\s,}]+)", re.IGNORECASE),
]


@dataclass(frozen=True)
class CommandResult:
    command: list[str]
    exit_code: int | None
    elapsed_ms: int
    stdout_bytes: int
    stderr_bytes: int
    stdout_sha256: str
    stderr_sha256: str
    stdout_preview: str
    stderr_preview: str
    error: str = ""


def utc_run_id() -> str:
    return dt.datetime.now(dt.UTC).strftime("%Y%m%d-%H%M%S") + "-client-acceptance"


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


PROMPT_VALUE_FLAGS = {"-q", "--query", "-z", "--oneshot", "--prompt", "-m", "--message"}


def shell_quote(args: list[str], api_key: str) -> str:
    import shlex

    redacted: list[str] = []
    redact_next = False
    for part in args:
        if redact_next:
            redacted.append("[REDACTED_PROMPT]")
            redact_next = False
            continue
        if part in PROMPT_VALUE_FLAGS:
            redacted.append(part)
            redact_next = True
            continue
        redacted.append("[REDACTED]" if part == api_key and api_key else part)
    return " ".join(shlex.quote(part) for part in redacted)


def sha256_text(text: str) -> str:
    return hashlib.sha256(text.encode("utf-8", errors="replace")).hexdigest()


def run_command(args: list[str], timeout: float, api_key: str, env: dict[str, str] | None = None) -> CommandResult:
    started = time.perf_counter()
    try:
        proc = subprocess.run(
            args,
            input="",
            text=True,
            capture_output=True,
            timeout=timeout,
            env=env,
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
            stdout_preview=stdout[:600],
            stderr_preview=stderr[:600],
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
            stdout_preview=stdout[:600],
            stderr_preview=stderr[:600],
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
            stdout_preview="",
            stderr_preview="",
            error=f"{type(exc).__name__}:{exc}",
        )


def command_exists(path_or_name: str) -> str | None:
    if "/" in path_or_name:
        return path_or_name if Path(path_or_name).exists() else None
    return shutil.which(path_or_name)


def discover_entries(args: argparse.Namespace) -> dict[str, Any]:
    hermes_candidates = [args.hermes_bin, DEFAULT_HERMES_BIN, "hermes"]
    openclaw_candidates = [args.openclaw_bin, DEFAULT_OPENCLAW_BIN, "openclaw"]
    hermes = next((found for candidate in hermes_candidates if (found := command_exists(candidate))), None)
    openclaw = next((found for candidate in openclaw_candidates if (found := command_exists(candidate))), None)
    return {
        "hermes_bin": hermes,
        "openclaw_bin": openclaw,
        "node_runtime": inspect_node_runtime(),
        "config_paths": inspect_config_paths(),
    }


def run_discovery_command(args: list[str], timeout: float = 5.0) -> tuple[int | None, str]:
    try:
        env = os.environ.copy()
        env["PATH"] = prepend_path(env.get("PATH", ""), USER_NODE22_BIN_DIR)
        proc = subprocess.run(args, text=True, capture_output=True, timeout=timeout, check=False, env=env)
        return proc.returncode, (proc.stdout or proc.stderr or "").strip()
    except Exception as exc:
        return None, f"{type(exc).__name__}:{exc}"


def inspect_node_runtime() -> dict[str, Any]:
    version_code, version_text = run_discovery_command(["node", "--version"])
    which_code, which_text = run_discovery_command(["bash", "-lc", "command -v node || true"])
    managers: dict[str, str] = {}
    for name in ["nvm", "fnm", "corepack", "volta", "asdf"]:
        code, text = run_discovery_command(["bash", "-lc", f"command -v {name} || true"])
        if code == 0 and text:
            managers[name] = text
    for name, path in {
        "nvm_script": "$HOME/.nvm/nvm.sh",
        "fnm_dir": "$HOME/.fnm",
        "volta_dir": "$HOME/.volta",
    }.items():
        code, text = run_discovery_command(["bash", "-lc", f"[ -e {path} ] && echo {path} || true"])
        if code == 0 and text:
            managers[name] = text
    node22_code, node22_text = run_discovery_command(
        [
            "bash",
            "-lc",
            "for p in $HOME/.local/node_modules/.bin/node $HOME/.nvm/versions/node/v22*/bin/node $HOME/.fnm/node-versions/v22*/installation/bin/node $HOME/.volta/tools/image/node/22*/bin/node $HOME/.cache/node-v22-temp/node-v22*/bin/node $HOME/.cache/node-v22*/bin/node /usr/local/lib/nodejs/node-v22*/bin/node /opt/node-v22*/bin/node; do [ -x \"$p\" ] && echo \"$p\"; done | head -20",
        ]
    )
    return {
        "node_version": version_text if version_code == 0 else f"unavailable:{version_text}",
        "node_path": which_text if which_code == 0 else "",
        "managers": managers,
        "node22_candidates": node22_text.splitlines() if node22_code == 0 and node22_text else [],
    }


def inspect_config_paths() -> list[dict[str, Any]]:
    paths = [
        Path.home() / ".hermes/config.yaml",
        Path.home() / ".openclaw/openclaw.json",
        Path.home() / ".openclaw-zenproxy-v46/openclaw.json",
        Path.home() / ".local/share/hermes-openai",
        Path.home() / ".local/state/hermes",
        Path.home() / ".config/systemd/user/hermes-openai.service",
    ]
    rows: list[dict[str, Any]] = []
    for path in paths:
        row: dict[str, Any] = {"path": str(path), "exists": path.exists()}
        if path.exists():
            row["kind"] = "dir" if path.is_dir() else "file"
            row["size"] = path.stat().st_size
            if path.is_file():
                row["relevant_lines"] = relevant_config_lines(path)
        rows.append(row)
    return rows


def relevant_config_lines(path: Path) -> list[str]:
    try:
        text = path.read_text(encoding="utf-8", errors="replace")
    except Exception as exc:
        return [f"read_error={type(exc).__name__}"]
    lines: list[str] = []
    for raw in text.splitlines():
        stripped = raw.strip()
        if not stripped or stripped.startswith("#"):
            continue
        lower = stripped.lower()
        if any(token in lower for token in ["api_key", "apikey", "token", "secret", "password", "authorization", "bearer"]):
            if ":" in stripped:
                lines.append(stripped.split(":", 1)[0] + ": [REDACTED]")
            elif "=" in stripped:
                lines.append(stripped.split("=", 1)[0] + "=[REDACTED]")
            else:
                lines.append("[REDACTED SECRET LINE]")
        elif any(token in lower for token in ["baseurl", "base_url", "url", "model", "provider", "endpoint"]):
            lines.append(redact(stripped)[:180])
        if len(lines) >= 24:
            break
    return lines


def http_json(method: str, url: str, key: str, payload: dict[str, Any] | None, timeout: float) -> dict[str, Any]:
    headers = {"Accept": "application/json", "Content-Type": "application/json"}
    if key:
        headers["Authorization"] = f"Bearer {key}"
    data = None if payload is None else json.dumps(payload).encode("utf-8")
    req = urllib.request.Request(url, data=data, headers=headers, method=method)
    started = time.perf_counter()
    status = 0
    body = ""
    error = ""
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            status = resp.getcode()
            body = resp.read().decode("utf-8", errors="replace")
    except urllib.error.HTTPError as exc:
        status = exc.code
        body = exc.read().decode("utf-8", errors="replace")
        error = f"HTTPError:{exc.code}"
    except Exception as exc:
        error = f"{type(exc).__name__}:{exc}"
    redacted = redact(body, [key])
    return {
        "status": status,
        "elapsed_ms": int((time.perf_counter() - started) * 1000),
        "body_bytes": len(redacted.encode("utf-8", errors="replace")),
        "body_sha256": sha256_text(redacted),
        "error": error,
    }


def build_hermes_chat_command(hermes_bin: str, args: argparse.Namespace) -> list[str]:
    command = [
        hermes_bin,
        "chat",
        "-q",
        "Reply with exactly one word: OK",
        "-m",
        args.model,
        "--max-turns",
        "1",
        "--ignore-rules",
        "--source",
        "client-acceptance",
    ]
    if args.hermes_provider:
        command.extend(["--provider", args.hermes_provider])
    return command


def openclaw_base_command(openclaw_bin: str, args: argparse.Namespace) -> list[str]:
    command = [openclaw_bin]
    if args.openclaw_profile:
        command.extend(["--profile", args.openclaw_profile])
    return command


def openclaw_model_id(args: argparse.Namespace) -> str:
    if args.openclaw_model:
        return args.openclaw_model
    return args.model


def build_openclaw_help_command(openclaw_bin: str, args: argparse.Namespace) -> list[str]:
    return [*openclaw_base_command(openclaw_bin, args), "--help"]


def build_openclaw_candidate_command(openclaw_bin: str, args: argparse.Namespace, help_text: str) -> list[str] | None:
    lower = help_text.lower()
    prompt = "Reply with exactly one word: OK"
    if "capability *" in lower or "infer *" in lower or "usage: openclaw" in lower:
        return [
            *openclaw_base_command(openclaw_bin, args),
            "capability",
            "model",
            "run",
            "--local",
            "--json",
            "--model",
            openclaw_model_id(args),
            "--prompt",
            prompt,
        ]
    if "-p, --prompt" in lower or "--prompt" in lower:
        return [*openclaw_base_command(openclaw_bin, args), "--prompt", prompt]
    if "-q, --query" in lower or "--query" in lower:
        return [*openclaw_base_command(openclaw_bin, args), "--query", prompt]
    if "--oneshot" in lower:
        return [*openclaw_base_command(openclaw_bin, args), "--oneshot", prompt]
    if " chat " in f" {lower} ":
        return [*openclaw_base_command(openclaw_bin, args), "chat", "-q", prompt]
    return None


def build_openclaw_models_status_command(openclaw_bin: str, args: argparse.Namespace) -> list[str]:
    return [*openclaw_base_command(openclaw_bin, args), "models", "status", "--json"]


def build_openclaw_capability_list_command(openclaw_bin: str, args: argparse.Namespace) -> list[str]:
    return [*openclaw_base_command(openclaw_bin, args), "capability", "list", "--json"]


def build_openclaw_agent_help_command(openclaw_bin: str, args: argparse.Namespace) -> list[str]:
    return [*openclaw_base_command(openclaw_bin, args), "agent", "--help"]


def build_openclaw_error_probe_command(openclaw_bin: str, args: argparse.Namespace) -> list[str]:
    return [*openclaw_base_command(openclaw_bin, args), "__client_acceptance_invalid_command__"]


def base_env(args: argparse.Namespace) -> dict[str, str]:
    env = os.environ.copy()
    env["PATH"] = prepend_path(env.get("PATH", ""), USER_NODE22_BIN_DIR)
    env.update(
        {
            "NEWAPI_BASE_URL": args.base_url,
            "OPENAI_BASE_URL": args.base_url.rstrip("/") + "/v1",
            "OPENAI_API_BASE": args.base_url.rstrip("/") + "/v1",
            "HERMES_INFERENCE_MODEL": args.model,
            "OPENCLAW_MODEL": args.model,
        }
    )
    if args.api_key:
        env["OPENAI_API_KEY"] = args.api_key
        env["NEWAPI_API_KEY"] = args.api_key
    if args.openclaw_config:
        env["OPENCLAW_CONFIG_PATH"] = args.openclaw_config
    if args.hermes_provider:
        env["HERMES_INFERENCE_PROVIDER"] = args.hermes_provider
    return env


def prepend_path(current: str, entry: str) -> str:
    parts = [part for part in current.split(os.pathsep) if part]
    return os.pathsep.join([entry, *[part for part in parts if part != entry]])


def result_to_dict(result: CommandResult, api_key: str, include_previews: bool = True) -> dict[str, Any]:
    return {
        "command": shell_quote(result.command, api_key),
        "exit_code": result.exit_code,
        "elapsed_ms": result.elapsed_ms,
        "stdout_bytes": result.stdout_bytes,
        "stderr_bytes": result.stderr_bytes,
        "stdout_sha256": result.stdout_sha256,
        "stderr_sha256": result.stderr_sha256,
        "stdout_preview": result.stdout_preview if include_previews else "",
        "stderr_preview": result.stderr_preview if include_previews else "",
        "error": result.error,
    }


def markdown_report(
    args: argparse.Namespace,
    run_id: str,
    discovery: dict[str, Any],
    planned: list[list[str]],
    results: list[dict[str, Any]],
    diagnostics: list[str],
) -> str:
    mode = "execute" if args.execute else "dry-run"
    lines = [
        "# OpenClaw / Hermes Client Acceptance",
        "",
        f"- run_id: `{run_id}`",
        f"- mode: `{mode}`",
        f"- base_url: `{args.base_url}`",
        f"- model: `{args.model}`",
        f"- hermes_provider: `{args.hermes_provider or 'config/default'}`",
        f"- openclaw_config: `{args.openclaw_config or 'default discovery'}`",
        f"- openclaw_profile: `{args.openclaw_profile or 'default'}`",
        f"- openclaw_model: `{openclaw_model_id(args)}`",
        "- api_key: `[REDACTED]`" if args.api_key else "- api_key: `missing`",
        "",
        "## Discovered Entries",
        "",
        f"- hermes_bin: `{discovery.get('hermes_bin') or 'not found'}`",
        f"- openclaw_bin: `{discovery.get('openclaw_bin') or 'not found'}`",
        "",
        "## Config Locations",
        "",
    ]
    for item in discovery.get("config_paths", []):
        lines.append(f"- `{item['path']}` exists={item['exists']} kind={item.get('kind', '-') } size={item.get('size', 0)}")
        for relevant in item.get("relevant_lines", []):
            lines.append(f"  - `{relevant}`")
    lines.extend(["", "## Planned Commands", ""])
    if planned:
        for command in planned:
            lines.append(f"- `{shell_quote(command, args.api_key)}`")
    else:
        lines.append("- none")
    node_runtime = discovery.get("node_runtime") or {}
    lines.extend(["", "## Node Runtime", ""])
    lines.append(f"- node_version: `{node_runtime.get('node_version', 'unknown')}`")
    lines.append(f"- node_path: `{node_runtime.get('node_path', '')}`")
    managers = node_runtime.get("managers") or {}
    if managers:
        for name, path in managers.items():
            lines.append(f"- {name}: `{path}`")
    else:
        lines.append("- managers: `none discovered`")
    candidates = node_runtime.get("node22_candidates") or []
    if candidates:
        for candidate in candidates:
            lines.append(f"- node22_candidate: `{candidate}`")
    else:
        lines.append("- node22_candidates: `none discovered`")
    lines.extend(["", "## Results", ""])
    if results:
        for result in results:
            lines.append(f"### {result['case_id']}")
            lines.append("")
            if "command" in result:
                lines.append(f"- command: `{result['command']}`")
                lines.append(f"- exit_code: `{result.get('exit_code')}`")
                lines.append(f"- elapsed_ms: `{result.get('elapsed_ms')}`")
                lines.append(f"- stdout_bytes: `{result.get('stdout_bytes')}` sha256=`{result.get('stdout_sha256')}`")
                lines.append(f"- stderr_bytes: `{result.get('stderr_bytes')}` sha256=`{result.get('stderr_sha256')}`")
                if result.get("error"):
                    lines.append(f"- error: `{result.get('error')}`")
                if result.get("expected_failure"):
                    lines.append("- expected_failure: `true`")
                if result.get("stdout_preview"):
                    lines.append("")
                    lines.append("stdout_preview:")
                    lines.append("```text")
                    lines.append(str(result["stdout_preview"]))
                    lines.append("```")
                if result.get("stderr_preview"):
                    lines.append("")
                    lines.append("stderr_preview:")
                    lines.append("```text")
                    lines.append(str(result["stderr_preview"]))
                    lines.append("```")
            else:
                lines.append(f"- status: `{result.get('status')}`")
                lines.append(f"- elapsed_ms: `{result.get('elapsed_ms')}`")
                lines.append(f"- body_bytes: `{result.get('body_bytes')}` sha256=`{result.get('body_sha256')}`")
                if result.get("error"):
                    lines.append(f"- error: `{result.get('error')}`")
            lines.append("")
    else:
        lines.append("- not executed")
    lines.extend(["## Diagnostics / Next Steps", ""])
    if diagnostics:
        lines.extend(f"- {item}" for item in diagnostics)
    else:
        lines.append("- No blocking diagnostics.")
    lines.append("")
    return "\n".join(lines)


def main() -> int:
    parser = argparse.ArgumentParser(description="Dry-run or execute WSL-local OpenClaw/Hermes client acceptance.")
    parser.add_argument("--execute", action="store_true", help="Run safe P0 probes. Default is diagnostic dry-run.")
    parser.add_argument("--run-id", default=os.getenv("CLIENT_ACCEPTANCE_RUN_ID") or utc_run_id())
    parser.add_argument("--out-dir", default="test-records/runs")
    parser.add_argument("--base-url", default=os.getenv("NEWAPI_BASE_URL", DEFAULT_BASE_URL))
    parser.add_argument("--api-key", default=os.getenv("NEWAPI_API_KEY", ""))
    parser.add_argument("--model", default=os.getenv("ZEN_TEST_MODEL", DEFAULT_MODEL))
    parser.add_argument("--hermes-bin", default=os.getenv("HERMES_BIN", DEFAULT_HERMES_BIN))
    parser.add_argument("--hermes-provider", default=os.getenv("HERMES_INFERENCE_PROVIDER", ""))
    parser.add_argument("--openclaw-bin", default=os.getenv("OPENCLAW_BIN", DEFAULT_OPENCLAW_BIN))
    parser.add_argument("--openclaw-config", default=os.getenv("OPENCLAW_CONFIG_PATH", DEFAULT_OPENCLAW_CONFIG if Path(DEFAULT_OPENCLAW_CONFIG).exists() else ""))
    parser.add_argument("--openclaw-profile", default=os.getenv("OPENCLAW_PROFILE", ""))
    parser.add_argument("--openclaw-model", default=os.getenv("OPENCLAW_MODEL", ""))
    parser.add_argument("--timeout", type=float, default=float(os.getenv("CLIENT_ACCEPTANCE_TIMEOUT", "45")))
    args = parser.parse_args()

    discovery = discover_entries(args)
    planned: list[list[str]] = []
    results: list[dict[str, Any]] = []
    diagnostics: list[str] = []

    if not args.api_key:
        diagnostics.append("NEWAPI_API_KEY/--api-key is missing; HTTP NewAPI probes will be skipped.")

    planned.append([sys.executable, str(Path(__file__).name), "--execute", "--base-url", args.base_url, "--model", args.model, "--api-key", "[REDACTED]"])

    hermes_bin = discovery.get("hermes_bin")
    if hermes_bin:
        planned.append([hermes_bin, "--help"])
        planned.append(build_hermes_chat_command(hermes_bin, args))
    else:
        diagnostics.append("Hermes executable was not found in --hermes-bin, /home/lenovo/.local/bin/hermes, or PATH.")

    openclaw_bin = discovery.get("openclaw_bin")
    openclaw_help_text = ""
    if openclaw_bin:
        help_command = build_openclaw_help_command(openclaw_bin, args)
        planned.append(help_command)
        if not args.execute:
            diagnostics.append("OpenClaw short-chat is gated until --execute can inspect openclaw --help and confirm a safe one-shot command shape.")
    else:
        diagnostics.append("OpenClaw executable was not found in --openclaw-bin, /home/lenovo/.local/node_modules/.bin/openclaw, or PATH.")

    if args.execute:
        env = base_env(args)
        if args.api_key:
            models_result = http_json("GET", args.base_url.rstrip("/") + "/v1/models", args.api_key, None, args.timeout)
            results.append({"case_id": "P0-http-models", **models_result})

            chat_payload = {
                "model": args.model,
                "stream": False,
                "messages": [{"role": "user", "content": "Reply with exactly one word: OK"}],
            }
            chat_result = http_json("POST", args.base_url.rstrip("/") + "/v1/chat/completions", args.api_key, chat_payload, args.timeout)
            results.append({"case_id": "P0-http-short-chat", **chat_result})
            if int(chat_result.get("status") or 0) >= 400:
                diagnostics.append(
                    f"HTTP short-chat returned status={chat_result.get('status')}; this confirms the local /v1 route was reached but upstream inference did not complete."
                )
        else:
            diagnostics.append("HTTP NewAPI probes skipped because NEWAPI_API_KEY/--api-key is missing.")

        if hermes_bin:
            help_result = run_command([hermes_bin, "--help"], args.timeout, args.api_key, env)
            results.append({"case_id": "P0-hermes-help", **result_to_dict(help_result, args.api_key)})
            chat_result_cmd = run_command(build_hermes_chat_command(hermes_bin, args), args.timeout, args.api_key, env)
            results.append({"case_id": "P0-hermes-short-chat", **result_to_dict(chat_result_cmd, args.api_key, include_previews=False)})
            hermes_text = f"{chat_result_cmd.stdout_preview}\n{chat_result_cmd.stderr_preview}".lower()
            if "permission denied" in hermes_text and "agent.log" in hermes_text:
                diagnostics.append("Hermes executable and help path work, but short-chat is blocked by a non-writable ~/.hermes/logs/agent.log; runner did not chmod or delete user state.")
            if chat_result_cmd.error or chat_result_cmd.exit_code not in (0,):
                diagnostics.append(
                    f"Hermes short-chat failed or timed out with exit_code={chat_result_cmd.exit_code} error={chat_result_cmd.error or 'none'}."
                )

        if openclaw_bin:
            openclaw_help = run_command(build_openclaw_help_command(openclaw_bin, args), args.timeout, args.api_key, env)
            results.append({"case_id": "P0-openclaw-help", **result_to_dict(openclaw_help, args.api_key)})
            openclaw_help_text = f"{openclaw_help.stdout_preview}\n{openclaw_help.stderr_preview}"
            models_status = run_command(build_openclaw_models_status_command(openclaw_bin, args), args.timeout, args.api_key, env)
            results.append({"case_id": "P1-openclaw-models-status", **result_to_dict(models_status, args.api_key)})
            capability_list = run_command(build_openclaw_capability_list_command(openclaw_bin, args), args.timeout, args.api_key, env)
            results.append({"case_id": "P1-openclaw-capability-list", **result_to_dict(capability_list, args.api_key)})
            agent_help = run_command(build_openclaw_agent_help_command(openclaw_bin, args), args.timeout, args.api_key, env)
            results.append({"case_id": "P1-openclaw-agent-help", **result_to_dict(agent_help, args.api_key)})
            candidate = build_openclaw_candidate_command(openclaw_bin, args, openclaw_help_text)
            if candidate:
                planned.append(candidate)
                openclaw_result = run_command(candidate, args.timeout, args.api_key, env)
                results.append({"case_id": "P0-openclaw-short-chat", **result_to_dict(openclaw_result, args.api_key, include_previews=False)})
                if openclaw_result.exit_code not in (0,):
                    diagnostics.append(
                        "OpenClaw short-chat executed with the configured model alias but failed; inspect stderr hash/bytes and compare with HTTP short-chat status before changing OpenClaw config again."
                    )
            else:
                diagnostics.append("OpenClaw command shape is not confirmed from help output; skipped short-chat execution.")
            error_probe = run_command(build_openclaw_error_probe_command(openclaw_bin, args), args.timeout, args.api_key, env)
            results.append(
                {
                    "case_id": "P1-openclaw-error-unknown-command",
                    "expected_failure": True,
                    **result_to_dict(error_probe, args.api_key),
                }
            )
            if "node.js v22.19+" in openclaw_help_text.lower():
                node_runtime = discovery.get("node_runtime") or {}
                node22_candidates = node_runtime.get("node22_candidates") or []
                suffix = f" discovered node22 candidates: {', '.join(node22_candidates)}." if node22_candidates else " no local node22 candidate was discovered via nvm/fnm/volta/common paths."
                diagnostics.append("OpenClaw is installed, but current Node runtime is below v22.19; OpenClaw probes are blocked without changing the system runtime." + suffix)

    run_dir = Path(args.out_dir) / args.run_id
    run_dir.mkdir(parents=True, exist_ok=True)
    report_path = run_dir / "client-acceptance.md"
    report_path.write_text(markdown_report(args, args.run_id, discovery, planned, results, diagnostics), encoding="utf-8")
    print(str(report_path))
    if args.execute:
        failed = [
            item
            for item in results
            if not item.get("expected_failure")
            and (int(item.get("status") or 0) >= 400 or item.get("error") or item.get("exit_code") not in (None, 0))
        ]
        return 1 if failed else 0
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
