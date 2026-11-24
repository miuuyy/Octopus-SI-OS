#!/usr/bin/env python3
import json
import pathlib
import subprocess
import sys
import unittest


SCRIPT = pathlib.Path(__file__).resolve().parents[1] / "verify.py"


def run_raw(stdin_text):
    completed = subprocess.run(
        [sys.executable, str(SCRIPT)],
        input=stdin_text,
        text=True,
        capture_output=True,
        check=True,
        timeout=10,
    )
    return json.loads(completed.stdout)


def verify(params, payload):
    payload_utf8 = payload if isinstance(payload, str) else json.dumps(payload)
    request = {
        "params": params,
        "artifact_label": "connector.operation.request",
        "payload_utf8": payload_utf8,
    }
    return run_raw(json.dumps(request))


class OperationCallAllowedVerifierTests(unittest.TestCase):
    def test_allowed_operation_passes_with_and_without_approval_policy(self):
        params = {"operations": ["connector.list", "connector.read"]}
        base = {
            "operation": "connector.read",
            "arguments": {"id": "item-1"},
            "correlation_id": "corr-1",
        }
        verdict = verify(params, base)
        self.assertTrue(verdict["passed"])
        self.assertEqual(verdict["details"]["handler"], "operation_call_allowed")
        self.assertEqual(verdict["details"]["operation"], "connector.read")

        with_approval = {**base, "approval_policy": "always"}
        verdict = verify(params, with_approval)
        self.assertTrue(verdict["passed"])
        self.assertEqual(verdict["details"]["operation"], "connector.read")

    def test_excluded_operation_fails(self):
        verdict = verify(
            {"operations": ["connector.list", "connector.read"]},
            {
                "operation": "connector.write",
                "arguments": {"body": "blocked"},
                "correlation_id": "corr-2",
            },
        )
        self.assertFalse(verdict["passed"])
        self.assertEqual(verdict["details"]["reason"], "operation_not_allowed")
        self.assertEqual(verdict["details"]["operation"], "connector.write")

    def test_duplicate_json_keys_are_rejected_in_request_and_payload(self):
        dup_payload = (
            '{"operation":"connector.write","operation":"connector.read",'
            '"arguments":{},"correlation_id":"corr-3"}'
        )
        verdict = verify({"operations": ["connector.read"]}, dup_payload)
        self.assertFalse(verdict["passed"])
        self.assertEqual(verdict["details"]["reason"], "payload_json_invalid")

        dup_request = (
            '{"params":{"operations":["connector.read"]},'
            '"params":{"operations":["connector.write"]},'
            '"payload_utf8":"{\\"operation\\":\\"connector.read\\",\\"arguments\\":{},\\"correlation_id\\":\\"corr-3\\"}"}'
        )
        verdict = run_raw(dup_request)
        self.assertFalse(verdict["passed"])
        self.assertEqual(verdict["details"]["reason"], "request_json_invalid")

    def test_payload_shape_and_field_validation_fail_closed(self):
        params = {"operations": ["connector.read"]}
        valid = {
            "operation": "connector.read",
            "arguments": {},
            "correlation_id": "corr-4",
        }
        cases = [
            ("non_object_payload", '["connector.read"]', "payload_not_object"),
            ("non_finite_number", '{"operation":"connector.read","arguments":{"x":NaN},"correlation_id":"corr-4"}', "payload_json_invalid"),
            ("extra_top_level_key", {**valid, "extra": True}, "payload_shape_invalid"),
            ("missing_correlation_id", {"operation": "connector.read", "arguments": {}}, "payload_shape_invalid"),
            ("non_object_arguments", {**valid, "arguments": ["bad"]}, "payload_arguments_not_object"),
            ("empty_correlation_id", {**valid, "correlation_id": "   "}, "payload_correlation_id_invalid"),
            ("empty_approval_policy", {**valid, "approval_policy": "  "}, "payload_approval_policy_invalid"),
            ("non_string_approval_policy", {**valid, "approval_policy": 123}, "payload_approval_policy_invalid"),
            ("untrimmed_operation", {**valid, "operation": " connector.read "}, "payload_operation_invalid"),
        ]
        for label, payload, expected_reason in cases:
            with self.subTest(case=label):
                verdict = verify(params, payload)
                self.assertFalse(verdict["passed"])
                self.assertEqual(verdict["details"]["reason"], expected_reason)

    def test_malformed_params_fail_closed(self):
        valid_payload = {
            "operation": "connector.read",
            "arguments": {},
            "correlation_id": "corr-5",
        }
        cases = [
            ("non_object_params", ["connector.read"], "params_not_object"),
            ("extra_param_key", {"operations": ["connector.read"], "extra": 1}, "params_shape_invalid"),
            ("empty_operations", {"operations": []}, "operations_not_nonempty_array"),
            ("duplicate_operations", {"operations": ["connector.read", "connector.read"]}, "operation_id_duplicate"),
            ("blank_operation", {"operations": [" "]}, "operation_id_invalid"),
            ("untrimmed_operation", {"operations": [" connector.read"]}, "operation_id_invalid"),
        ]
        for label, params, expected_reason in cases:
            with self.subTest(case=label):
                verdict = verify(params, valid_payload)
                self.assertFalse(verdict["passed"])
                self.assertEqual(verdict["details"]["reason"], expected_reason)


if __name__ == "__main__":
    unittest.main()
