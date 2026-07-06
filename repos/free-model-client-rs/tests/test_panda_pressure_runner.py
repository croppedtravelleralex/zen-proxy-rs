import importlib.util
import json
import os
import sys
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts" / "panda_pressure_runner.py"


def load_runner():
    spec = importlib.util.spec_from_file_location("panda_pressure_runner", SCRIPT)
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


class PandaPressureRunnerHarnessTests(unittest.TestCase):
    def test_claude_settings_includes_auth_env_when_key_is_provided(self):
        runner = load_runner()

        settings = json.loads(
            runner.claude_settings_json(
                "http://127.0.0.1:18082",
                "mimo-v2.5",
                "sk-test-secret",
            )
        )

        self.assertEqual(settings["env"]["ANTHROPIC_BASE_URL"], "http://127.0.0.1:18082")
        self.assertEqual(settings["env"]["ANTHROPIC_MODEL"], "mimo-v2.5")
        self.assertEqual(settings["env"]["ANTHROPIC_API_KEY"], "sk-test-secret")
        self.assertEqual(settings["env"]["ANTHROPIC_AUTH_TOKEN"], "sk-test-secret")

    def test_path_for_wsl_converts_hermes_unc_workspace(self):
        runner = load_runner()

        self.assertEqual(
            runner.path_for_wsl(
                Path(r"\\wsl.localhost\HermesUbuntu\home\lenovo\free-model-client-rs")
            ),
            "/home/lenovo/free-model-client-rs",
        )

    def test_policy_plan_covers_required_cases_and_protocols(self):
        runner = load_runner()

        plan = runner.build_policy_plan(
            "policy-smoke",
            ["deepseek-v4-flash", "big-pickle"],
        )

        self.assertEqual({case.protocol for case in plan}, {"openai", "anthropic"})
        self.assertTrue(
            {
                "flash_input_room",
                "flash_output_room",
                "lite_not_claudecode",
                "provider_usage_probe",
                "cache_probe",
            }.issubset({case.case_type for case in plan})
        )
        lite_cases = [case for case in plan if case.case_type == "lite_not_claudecode"]
        self.assertTrue(lite_cases)
        self.assertTrue(all(case.model == "big-pickle" for case in lite_cases))
        self.assertTrue(all(case.client_header == "claude-code" for case in lite_cases))
        self.assertTrue(all(case.expected_source_client == "claude-code" for case in lite_cases))
        self.assertTrue(all(case.expected_effective_client == "unknown" for case in lite_cases))
        self.assertTrue(all(case.tools for case in lite_cases))

    def test_policy_plan_respects_single_model_key_scope(self):
        runner = load_runner()

        for model in ["deepseek-v4-flash", "mimo-v2.5", "big-pickle"]:
            with self.subTest(model=model):
                plan = runner.build_policy_plan("policy-smoke", [model])

                self.assertTrue(plan)
                self.assertEqual({case.model for case in plan}, {model})
                self.assertEqual({case.protocol for case in plan}, {"openai", "anthropic"})
                self.assertTrue(
                    {"provider_usage_probe", "cache_probe"}.issubset(
                        {case.case_type for case in plan}
                    )
                )

    def test_claudecode_timeout_takes_precedence_over_stream_text_auth_words(self):
        runner = load_runner()
        tmp_dir = tempfile.TemporaryDirectory()
        self.addCleanup(tmp_dir.cleanup)
        tmp_path = Path(tmp_dir.name)
        workspace = runner.prepare_workspace(tmp_path)

        def fake_run_wsl_claudecode(case, model, prompt_text, case_workspace, base_url, key, timeout_ms):
            return {
                "ok": False,
                "returncode": 124,
                "timed_out": False,
                "total_ms": timeout_ms,
                "first_stdout_ms": 10,
                "stdout": '{"type":"system","permissionMode":"bypassPermissions"}\nunauthorized metadata',
                "stderr": "",
                "result": "OK",
                "usage": None,
                "tool_call_count": 0,
                "config_mode": "wsl-interop-env-settings",
            }

        original = runner.run_wsl_claudecode
        runner.run_wsl_claudecode = fake_run_wsl_claudecode
        try:
            row = runner.run_case(
                "wsl-claudecode",
                runner.CaseSpec("short_stream", "short", stream=True),
                0,
                "deepseek-v4-flash",
                workspace,
                "http://100.69.228.93:8081",
                "sk-test-secret",
                180000,
                tmp_path,
                ["deepseek-v4-flash"],
            )
        finally:
            runner.run_wsl_claudecode = original

        self.assertEqual(row["status"], "error")
        self.assertEqual(row["error_class"], "client_timeout")
        self.assertEqual(row["returncode"], 124)

    def test_cache_observation_classifies_four_states(self):
        runner = load_runner()

        self.assertEqual(
            runner.classify_cache_observation(
                True,
                200,
                {"cache_read_input_tokens": 7, "cache_creation_input_tokens": 0},
                "",
            ),
            "accepted",
        )
        self.assertEqual(
            runner.classify_cache_observation(
                True,
                200,
                {"cache_read_input_tokens": 0, "cache_creation_input_tokens": 0},
                "",
            ),
            "attempted",
        )
        self.assertEqual(
            runner.classify_cache_observation(True, 400, None, "cache_control is not supported"),
            "rejected",
        )
        self.assertEqual(runner.classify_cache_observation(False, 200, None, ""), "ignored")
        self.assertEqual(
            runner.classify_cache_observation(True, 200, {"prompt_tokens": 10}, ""),
            "ignored",
        )

    def test_policy_case_extracts_provider_and_usage_signals(self):
        runner = load_runner()
        tmp_path = ROOT / ".codex_tmp" / "unit-policy-harness"

        def fake_http_exchange(method, url, key, payload, timeout_s, extra_headers=None):
            self.assertEqual(method, "POST")
            self.assertEqual(key, "sk-test-secret")
            self.assertEqual(extra_headers, {"x-fmc-client": "openai-sdk"})
            return {
                "status_code": 200,
                "raw_text": json.dumps(
                    {
                        "choices": [
                            {
                                "message": {"role": "assistant", "content": "CACHE_POLICY_OK"},
                                "finish_reason": "stop",
                            }
                        ],
                        "usage": {
                            "prompt_tokens": 120,
                            "completion_tokens": 8,
                            "total_tokens": 128,
                            "prompt_tokens_details": {"cached_tokens": 33},
                            "cache_read_input_tokens": 33,
                            "cache_creation_input_tokens": 12,
                        },
                    }
                ),
                "total_ms": 42,
                "protocol_first_byte_ms": 10,
                "headers": {"x-zen-observed-exit-ip": "203.0.113.10"},
            }

        original = runner.http_exchange
        runner.http_exchange = fake_http_exchange
        try:
            case = runner.PolicyCaseSpec(
                case_type="cache_probe",
                protocol="openai",
                model="deepseek-v4-flash",
                stream=False,
                client_header="openai-sdk",
                max_tokens=256,
                prompt_target_tokens=128,
                cache_attempted=True,
                expected_source_client="openai-sdk",
                expected_effective_client="openai-sdk",
            )
            row = runner.run_policy_case(
                case,
                0,
                "http://100.69.228.93:8081",
                "sk-test-secret",
                30000,
                tmp_path,
            )
        finally:
            runner.http_exchange = original

        self.assertIs(row["policy_ok"], True)
        self.assertIs(row["provider_header_signal"], True)
        self.assertEqual(row["provider_header_names"], ["x-zen-observed-exit-ip"])
        self.assertIs(row["provider_body_usage_signal"], True)
        self.assertEqual(row["usage_input_tokens"], 120)
        self.assertEqual(row["usage_output_tokens"], 8)
        self.assertEqual(row["usage_cached_tokens"], 33)
        self.assertEqual(row["cache_observation"], "accepted")
        self.assertEqual(len(row["request_shape_hash"]), 16)
        self.assertIs(row["redaction_ok"], True)

    def test_anthropic_stream_parser_merges_body_usage(self):
        runner = load_runner()
        raw = "\n".join(
            [
                'event: message_start\ndata: {"type":"message_start","message":{"usage":{"input_tokens":55,"output_tokens":0}}}',
                'event: content_block_delta\ndata: {"type":"content_block_delta","delta":{"type":"text_delta","text":"hello"}}',
                'event: message_delta\ndata: {"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":4,"cache_read_input_tokens":2}}',
            ]
        )

        result, usage, tool_count, finish_reason = runner.parse_policy_response(
            "anthropic",
            True,
            raw,
        )

        self.assertEqual(result, "hello")
        self.assertEqual(usage["input_tokens"], 55)
        self.assertEqual(usage["output_tokens"], 4)
        self.assertEqual(usage["cache_read_input_tokens"], 2)
        self.assertEqual(tool_count, 0)
        self.assertEqual(finish_reason, "end_turn")

    def test_observability_summary_groups_latency_and_token_weighted_cache(self):
        runner = load_runner()

        rows = [
            {
                "model": "deepseek-v4-flash",
                "prompt_est_tokens": 12_000,
                "stream": True,
                "cache_observation": "accepted",
                "protocol_first_byte_ms": 10,
                "first_content_ms": 100,
                "first_tool_call_ms": None,
                "first_tool_emit_ms": None,
                "total_ms": 1000,
                "cache_read_input_tokens": 90,
                "cache_miss_input_tokens": 10,
                "status": "ok",
                "semantic_ok": True,
                "tool_success": True,
                "error_class": "ok",
            },
            {
                "model": "deepseek-v4-flash",
                "prompt_est_tokens": 12_500,
                "stream": True,
                "cache_observation": "accepted",
                "protocol_first_byte_ms": 20,
                "first_content_ms": 200,
                "first_tool_call_ms": 220,
                "first_tool_emit_ms": 260,
                "total_ms": 2000,
                "cache_read_input_tokens": 810,
                "cache_miss_input_tokens": 90,
                "status": "ok",
                "semantic_ok": True,
                "tool_success": True,
                "error_class": "ok",
            },
            {
                "model": "mimo-v2.5",
                "prompt_est_tokens": 120_000,
                "stream": False,
                "cache_observation": "rejected",
                "protocol_first_byte_ms": 30,
                "first_content_ms": 300,
                "total_ms": 3000,
                "cache_read_input_tokens": 0,
                "cache_miss_input_tokens": 1000,
                "status": "error",
                "semantic_ok": False,
                "tool_success": False,
                "error_class": "empty_upstream",
            },
        ]

        summary = runner.observability_summary(rows)

        group_key = "model=deepseek-v4-flash|bucket=10k-50k|stream=true|cache=accepted"
        group = summary["groups"][group_key]
        self.assertEqual(group["total"], 2)
        self.assertEqual(group["quality_pass_rate"], 100.0)
        self.assertEqual(group["cache_token_read_pct"], 90.0)
        self.assertEqual(group["latency_ms"]["protocol_first_byte_ms"]["p50"], 10)
        self.assertEqual(group["latency_ms"]["protocol_first_byte_ms"]["p95"], 20)
        self.assertEqual(group["latency_ms"]["first_tool_emit_ms"]["p50"], 260)
        self.assertEqual(
            summary["groups"]["model=mimo-v2.5|bucket=100k-200k|stream=false|cache=rejected"][
                "errors"
            ],
            {"empty_upstream": 1},
        )

    def test_policy_case_adds_prefix_hashes_and_cache_dataset_fields(self):
        runner = load_runner()
        tmp_path = ROOT / ".codex_tmp" / "unit-policy-observability"

        def fake_http_exchange(method, url, key, payload, timeout_s, extra_headers=None):
            return {
                "status_code": 200,
                "raw_text": json.dumps(
                    {
                        "choices": [
                            {
                                "message": {"role": "assistant", "content": "CACHE_POLICY_OK"},
                                "finish_reason": "stop",
                            }
                        ],
                        "usage": {
                            "prompt_tokens": 100,
                            "completion_tokens": 5,
                            "prompt_cache_hit_tokens": 80,
                            "prompt_cache_miss_tokens": 20,
                        },
                    }
                ),
                "total_ms": 123,
                "protocol_first_byte_ms": 11,
                "first_content_ms": 77,
                "first_tool_call_ms": None,
                "first_tool_emit_ms": None,
                "headers": {},
            }

        original = runner.http_exchange
        runner.http_exchange = fake_http_exchange
        try:
            case = runner.PolicyCaseSpec(
                case_type="cache_probe",
                protocol="openai",
                model="deepseek-v4-flash",
                stream=False,
                client_header="openai-sdk",
                max_tokens=256,
                prompt_target_tokens=4096,
                cache_attempted=True,
                expected_source_client="openai-sdk",
                expected_effective_client="openai-sdk",
            )
            row = runner.run_policy_case(
                case,
                0,
                "http://100.69.228.93:8081",
                "sk-test-secret",
                30000,
                tmp_path,
            )
        finally:
            runner.http_exchange = original

        self.assertEqual(row["cache_observation"], "accepted")
        self.assertEqual(row["cache_read_input_tokens"], 80)
        self.assertEqual(row["cache_miss_input_tokens"], 20)
        self.assertEqual(row["cache_token_read_pct"], 80.0)
        self.assertEqual(row["prompt_bucket"], "lt_10k")
        self.assertEqual(len(row["prompt_hash"]), 16)
        self.assertEqual(len(row["prefix_4k_hash"]), 16)
        self.assertGreater(row["cache_material_bytes"], 4096)

    def test_cache_pressure_plan_writes_manifest_without_key_or_network(self):
        runner = load_runner()
        old_env = {name: os.environ.pop(name, None) for name in runner.KEY_ENV_NAMES}

        def forbidden_http_json(*args, **kwargs):
            raise AssertionError("cache-pressure-plan must not perform network IO")

        original_http_json = runner.http_json
        runner.http_json = forbidden_http_json
        try:
            with tempfile.TemporaryDirectory() as tmpdir:
                rc = runner.main(
                    [
                        "--mode",
                        "cache-pressure-plan",
                        "--run-dir",
                        tmpdir,
                        "--models",
                        "deepseek-v4-flash,mimo-v2.5",
                        "--cache-pressure-rpm",
                        "50",
                        "--cache-pressure-duration-minutes",
                        "5",
                    ]
                )
                manifest_path = Path(tmpdir) / "cache-pressure-manifest.json"
                dataset_schema_path = Path(tmpdir) / "dataset-schema.json"
                manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
                dataset_schema = json.loads(dataset_schema_path.read_text(encoding="utf-8"))
        finally:
            runner.http_json = original_http_json
            for name, value in old_env.items():
                if value is not None:
                    os.environ[name] = value

        self.assertEqual(rc, 0)
        self.assertEqual(manifest["run_mode"], "plan_only")
        self.assertEqual(manifest["total_planned_requests"], 2000)
        self.assertFalse((Path(tmpdir) / "raw-results.jsonl").exists())
        self.assertIn("first_tool_emit_ms", dataset_schema["latency_fields"])
        self.assertIn("cache_token_read_pct", dataset_schema["cache_fields"])


if __name__ == "__main__":
    unittest.main()
