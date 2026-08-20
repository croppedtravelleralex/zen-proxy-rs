#!/usr/bin/env python3
"""Smoke zen-proxy-rs-test (:4011) truncation guards via temporary mock upstream.

Runs ON panda. Temporarily points test instance to a local mock Zen upstream
(ALLOW_DIRECT_FALLBACK=true) to exercise DSML + unfinished-tool-intent retry
paths without touching production :4001/:4002/:4004.

Restores systemd override and restarts zen-proxy-rs-test on exit.
"""
from __future__ import annotations

import json
import os
import signal
import subprocess
import sys
import threading
import time
import urllib.error
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

MOCK_PORT = 41999
PROXY_PORT = 4011
OVERRIDE_PATH = "/etc/systemd/system/zen-proxy-rs-test.service.d/smoke-mock-upstream.conf"
EMPTY_NODES_PATH = "/tmp/smoke-empty-nodes.json"
OVERRIDES_ENV_PATH = "/tmp/smoke-zen-overrides.env"
DSML = "｜DSML｜"

request_lock = threading.Lock()
request_count = 0
mock_server: ThreadingHTTPServer | None = None


def load_proxy_key() -> str:
    with open("/etc/zen-proxy-rs/common.env", encoding="utf-8") as fh:
        for line in fh:
            if line.startswith("PROXY_API_KEY="):
                return line.split("=", 1)[1].strip()
    raise RuntimeError("PROXY_API_KEY not found in common.env")


class MockZenHandler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, fmt: str, *args) -> None:
        sys.stderr.write(f"[mock-zen] {self.address_string()} {fmt % args}\n")

    def do_POST(self) -> None:
        global request_count
        length = int(self.headers.get("Content-Length", "0"))
        raw = self.rfile.read(length)
        wire_body = raw.decode("utf-8", errors="replace")

        with request_lock:
            request_count += 1
            count = request_count

        if "/chat/completions" not in self.path:
            self.send_error(404, "not found")
            return

        def sse(chunk: dict) -> bytes:
            payload = f"data: {json.dumps(chunk, ensure_ascii=False)}\n\n"
            return (payload + "data: [DONE]\n\n").encode("utf-8")

        body: bytes
        if "dsml-truncation-retry" in wire_body:
            if count == 1:
                chunk = {
                    "choices": [{
                        "delta": {
                            "reasoning_content": (
                                f"</{DSML}parameter>\n</{DSML}invoke>\n</{DSML}tool_calls>"
                            ),
                        },
                        "finish_reason": "stop",
                    }],
                }
            else:
                chunk = {
                    "choices": [{
                        "delta": {
                            "tool_calls": [{
                                "index": 0,
                                "id": "call_dsml_retry_1",
                                "type": "function",
                                "function": {
                                    "name": "Bash",
                                    "arguments": '{"command":"pwd"}',
                                },
                            }],
                        },
                        "finish_reason": "tool_calls",
                    }],
                }
            body = sse(chunk)
        elif "unfinished-tool-intent-retry" in wire_body:
            if count == 1:
                chunk = {
                    "choices": [{
                        "delta": {"content": "命令超时。检查输出文件是否已生成"},
                        "finish_reason": "stop",
                    }],
                }
            else:
                chunk = {
                    "choices": [{
                        "delta": {
                            "tool_calls": [{
                                "index": 0,
                                "id": "call_intent_retry_1",
                                "type": "function",
                                "function": {
                                    "name": "Bash",
                                    "arguments": (
                                        '{"command":"test -f /tmp/admin_disk_check.txt"}'
                                    ),
                                },
                            }],
                        },
                        "finish_reason": "tool_calls",
                    }],
                }
            body = sse(chunk)
        else:
            chunk = {
                "choices": [{
                    "delta": {"content": "zen v4 ok"},
                    "finish_reason": "stop",
                }],
            }
            body = sse(chunk)

        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("x-zen-observed-exit-ip", "direct")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


def start_mock() -> None:
    global mock_server
    mock_server = ThreadingHTTPServer(("127.0.0.1", MOCK_PORT), MockZenHandler)
    thread = threading.Thread(target=mock_server.serve_forever, daemon=True)
    thread.start()
    time.sleep(0.2)


def stop_mock() -> None:
    global mock_server
    if mock_server is not None:
        mock_server.shutdown()
        mock_server = None


def sh(cmd: str) -> str:
    return subprocess.check_output(cmd, shell=True, text=True).strip()


def apply_override() -> None:
    with open(EMPTY_NODES_PATH, "w", encoding="utf-8") as fh:
        fh.write("[]\n")
    overrides = (
        f"NODES_FILE={EMPTY_NODES_PATH}\n"
        f"UPSTREAM_BASE=http://127.0.0.1:{MOCK_PORT}/zen\n"
        "ALLOW_DIRECT_FALLBACK=true\n"
        "POOL_MAX_RETRIES=0\n"
        "INSTANCE_ID=panda-zen-smoke-tmp\n"
    )
    with open(OVERRIDES_ENV_PATH, "w", encoding="utf-8") as fh:
        fh.write(overrides)
    os.makedirs(os.path.dirname(OVERRIDE_PATH), exist_ok=True)
    with open(OVERRIDE_PATH, "w", encoding="utf-8") as fh:
        fh.write("[Service]\n")
        fh.write(f"EnvironmentFile=-{OVERRIDES_ENV_PATH}\n")
    sh("systemctl daemon-reload")
    sh("systemctl restart zen-proxy-rs-test")
    deadline = time.time() + 30
    while time.time() < deadline:
        try:
            with urllib.request.urlopen(
                f"http://127.0.0.1:{PROXY_PORT}/health", timeout=3
            ) as resp:
                if resp.status == 200:
                    return
        except urllib.error.URLError:
            pass
        time.sleep(1)
    raise RuntimeError("zen-proxy-rs-test did not become healthy after override")


def remove_override() -> None:
    if os.path.exists(OVERRIDE_PATH):
        os.remove(OVERRIDE_PATH)
    if os.path.exists(EMPTY_NODES_PATH):
        os.remove(EMPTY_NODES_PATH)
    if os.path.exists(OVERRIDES_ENV_PATH):
        os.remove(OVERRIDES_ENV_PATH)
    sh("systemctl daemon-reload")
    sh("systemctl restart zen-proxy-rs-test")


def anthropic_stream(prompt: str, api_key: str) -> tuple[int, str]:
    payload = {
        "model": "deepseek-v4-flash",
        "messages": [{"role": "user", "content": prompt}],
        "max_tokens": 64,
        "stream": True,
        "tools": [{
            "name": "Bash",
            "description": "Run a shell command",
            "input_schema": {
                "type": "object",
                "properties": {"command": {"type": "string"}},
                "required": ["command"],
            },
        }],
    }
    body = json.dumps(payload).encode("utf-8")
    req = urllib.request.Request(
        f"http://127.0.0.1:{PROXY_PORT}/v1/messages",
        data=body,
        headers={
            "Authorization": f"Bearer {api_key}",
            "Content-Type": "application/json",
            "x-fmc-client": "claude-code",
        },
        method="POST",
    )
    with urllib.request.urlopen(req, timeout=60) as resp:
        text = resp.read().decode("utf-8", errors="replace")
        return resp.status, text


def openai_stream(prompt: str, api_key: str) -> tuple[int, str]:
    payload = {
        "model": "deepseek-v4-flash",
        "messages": [{"role": "user", "content": prompt}],
        "stream": True,
        "tools": [{
            "type": "function",
            "function": {
                "name": "Bash",
                "parameters": {
                    "type": "object",
                    "properties": {"command": {"type": "string"}},
                    "required": ["command"],
                },
            },
        }],
    }
    body = json.dumps(payload).encode("utf-8")
    req = urllib.request.Request(
        f"http://127.0.0.1:{PROXY_PORT}/v1/chat/completions",
        data=body,
        headers={
            "Authorization": f"Bearer {api_key}",
            "Content-Type": "application/json",
            "x-fmc-client": "claude-code",
        },
        method="POST",
    )
    with urllib.request.urlopen(req, timeout=60) as resp:
        text = resp.read().decode("utf-8", errors="replace")
        return resp.status, text


def check(name: str, ok: bool, detail: str) -> None:
    mark = "PASS" if ok else "FAIL"
    print(f"{mark} {name}: {detail}")
    if not ok:
        raise AssertionError(f"{name}: {detail}")


def main() -> None:
    api_key = load_proxy_key()
    restored = False

    def cleanup(_signum=None, _frame=None) -> None:
        nonlocal restored
        if restored:
            return
        restored = True
        stop_mock()
        try:
            remove_override()
            print("RESTORED zen-proxy-rs-test upstream override")
        except Exception as exc:
            print(f"RESTORE ERROR: {exc}", file=sys.stderr)

    signal.signal(signal.SIGINT, cleanup)
    signal.signal(signal.SIGTERM, cleanup)

    try:
        print("=== smoke_4011_truncation_guards ===")
        health = sh(f"curl -sf http://127.0.0.1:{PROXY_PORT}/health")
        print("health_before:", health)

        start_mock()
        apply_override()
        health = sh(f"curl -sf http://127.0.0.1:{PROXY_PORT}/health")
        print("health_mock:", health)
        pools = json.loads(health).get("pools", {})
        check(
            "dispatch_pool_empty",
            pools.get("dispatch", 0) == 0,
            f"pools={pools}",
        )

        global request_count
        request_count = 0

        status, body = anthropic_stream("dsml-truncation-retry", api_key)
        check("anthropic_dsml_status", status == 200, f"status={status}")
        check("anthropic_dsml_no_leak", "DSML" not in body, "DSML leaked")
        check("anthropic_dsml_tool", '"type":"tool_use"' in body, "no tool_use")
        check("anthropic_dsml_bash", '"name":"Bash"' in body, "no Bash")
        check("anthropic_dsml_pwd", "pwd" in body, "no pwd in tool args")
        check(
            "anthropic_dsml_stop",
            '"stop_reason":"tool_use"' in body,
            "stop_reason not tool_use",
        )
        check(
            "anthropic_dsml_retries",
            request_count >= 2,
            f"upstream_calls={request_count}",
        )

        request_count = 0
        status, body = anthropic_stream("unfinished-tool-intent-retry", api_key)
        check("intent_status", status == 200, f"status={status}")
        check("intent_no_fake_text", "命令超时" not in body, "fake end_turn text leaked")
        check("intent_tool", '"type":"tool_use"' in body, "no tool_use")
        check("intent_bash", '"name":"Bash"' in body, "no Bash")
        check("intent_path", "admin_disk_check.txt" in body, "expected command missing")
        check(
            "intent_stop",
            '"stop_reason":"tool_use"' in body,
            "stop_reason not tool_use",
        )
        check(
            "intent_retries",
            request_count >= 2,
            f"upstream_calls={request_count}",
        )

        request_count = 0
        status, body = openai_stream("dsml-truncation-retry", api_key)
        check("openai_dsml_status", status == 200, f"status={status}")
        check("openai_dsml_no_leak", "DSML" not in body, "DSML leaked")
        check("openai_dsml_bash", '"name":"Bash"' in body, "no Bash")
        check("openai_dsml_pwd", "pwd" in body, "no pwd")
        check(
            "openai_dsml_retries",
            request_count >= 2,
            f"upstream_calls={request_count}",
        )

        print("ALL PASS smoke_4011_truncation_guards")
    finally:
        cleanup()


if __name__ == "__main__":
    try:
        main()
    except Exception as exc:
        print(f"SMOKE FAILED: {exc}", file=sys.stderr)
        sys.exit(1)
