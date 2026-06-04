import importlib.util
import json
import sys
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
    def test_policy_plan_covers_required_cases_and_protocols(self):
        runner = load_runner()

        plan = runner.build_policy_plan(
            "policy-smoke",
            ["deepseek-v4-flash", "deepseek-v4-flash-lite"],
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
        self.assertTrue(all(case.model == "deepseek-v4-flash-lite" for case in lite_cases))
        self.assertTrue(all(case.client_header == "claude-code" for case in lite_cases))
        self.assertTrue(all(case.expected_source_client == "claude-code" for case in lite_cases))
        self.assertTrue(all(case.expected_effective_client == "unknown" for case in lite_cases))
        self.assertTrue(all(case.tools for case in lite_cases))

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


if __name__ == "__main__":
    unittest.main()
