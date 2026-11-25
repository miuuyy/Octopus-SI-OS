#!/usr/bin/env python3
import json
import os
import pathlib
import subprocess
import sys
import tempfile
import unittest


SCRIPT = pathlib.Path(__file__).resolve().parents[1] / "verify.py"


def run_verifier(params, payload_bytes, raw_request=None):
    with tempfile.TemporaryDirectory() as tmpdir:
        payload_path = pathlib.Path(tmpdir) / "payload"
        if payload_bytes is not None:
            payload_path.write_bytes(payload_bytes)
        if raw_request is None:
            try:
                payload_utf8 = payload_bytes.decode("utf-8") if payload_bytes is not None else None
            except UnicodeDecodeError:
                payload_utf8 = None
            request_text = json.dumps(
                {
                    "run": "run_test",
                    "checkpoint": "cp_test",
                    "epoch": 0,
                    "mark": "artifact.schema.valid",
                    "artifact": {"id": "art_test", "content": "0" * 64},
                    "artifact_label": "report.json",
                    "params": params,
                    "metadata": {},
                    "payload_path": str(payload_path) if payload_bytes is not None else None,
                    "payload_utf8": payload_utf8,
                }
            )
        else:
            request_text = raw_request
        env = {"PATH": os.environ.get("PATH", ""), "TMPDIR": tmpdir, "PYTHONDONTWRITEBYTECODE": "1"}
        completed = subprocess.run(
            [sys.executable, "-B", str(SCRIPT)],
            input=request_text,
            text=True,
            capture_output=True,
            check=True,
            timeout=10,
            cwd=tmpdir,
            env=env,
        )
        return json.loads(completed.stdout)


class JsonSchemaVerifierTests(unittest.TestCase):
    def test_combinators_const_enum_and_unique_items(self):
        schema = {
            "title": "Decision",
            "description": "Structured decision record",
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object",
            "required": ["version", "tags", "choice", "score"],
            "minProperties": 4,
            "maxProperties": 5,
            "properties": {
                "version": {"const": 1},
                "tags": {"type": "array", "uniqueItems": True, "minItems": 2, "maxItems": 4},
                "choice": {
                    "oneOf": [
                        {"type": "string", "minLength": 2, "maxLength": 10},
                        {"type": "integer", "exclusiveMinimum": 0, "exclusiveMaximum": 10},
                    ]
                },
                "score": {
                    "allOf": [
                        {"type": ["integer", "number"]},
                        {"minimum": 0, "maximum": 1.5},
                    ]
                },
                "extra": {
                    "anyOf": [
                        {"type": "null"},
                        {"type": "boolean"},
                    ]
                },
            },
            "additionalProperties": {"type": "string"},
        }
        payload = json.dumps(
            {
                "version": 1,
                "tags": [1, True, "alpha"],
                "choice": 5,
                "score": 1.25,
                "extra": None,
            }
        ).encode("utf-8")
        verdict = run_verifier({"schema": schema, "timeout_ms": 1500}, payload)
        self.assertTrue(verdict["passed"], verdict)
        self.assertEqual(verdict["details"]["handler"], "json_schema")
        self.assertEqual(verdict["details"]["root_type"], "object")

    def test_strict_type_distinctions_in_const_enum_and_unique_items(self):
        v1 = run_verifier({"schema": {"const": 1}}, b"true")
        self.assertFalse(v1["passed"])
        self.assertEqual(v1["details"]["keyword"], "const")

        v2 = run_verifier({"schema": {"enum": [0, 1]}}, b"false")
        self.assertFalse(v2["passed"])
        self.assertEqual(v2["details"]["keyword"], "enum")

        v3 = run_verifier({"schema": {"type": "array", "uniqueItems": True}}, b"[1, 1.0]")
        self.assertFalse(v3["passed"])
        self.assertEqual(v3["details"]["keyword"], "uniqueItems")

        v4 = run_verifier({"schema": {"type": "array", "uniqueItems": True}}, b"[1, true]")
        self.assertTrue(v4["passed"])

    def test_unsupported_keywords_fail_closed_recursively(self):
        for schema, expected_keyword, payload in [
            (
                {
                    "type": "object",
                    "properties": {
                        "nested": {"$ref": "#/definitions/foo"},
                    },
                },
                "$ref",
                b'{"nested":{}}',
            ),
            (
                {"type": "string", "pattern": "^[a-z]+$"},
                "pattern",
                b'"abc"',
            ),
        ]:
            with self.subTest(keyword=expected_keyword):
                verdict = run_verifier({"schema": schema}, payload)
                self.assertFalse(verdict["passed"])
                self.assertEqual(verdict["details"]["reason"], "schema_keyword_unsupported")
                self.assertEqual(verdict["details"]["keyword"], expected_keyword)

    def test_invalid_schema_bounds_fail_closed(self):
        cases = [
            {"type": "string", "minLength": 5, "maxLength": 2},
            {"type": "number", "minimum": 10, "maximum": 5},
            {"type": ["string", "string"]},
            {"enum": []},
            {"oneOf": []},
        ]
        for schema in cases:
            with self.subTest(schema=schema):
                verdict = run_verifier({"schema": schema}, b"{}")
                self.assertFalse(verdict["passed"])
                self.assertEqual(verdict["details"]["reason"], "schema_invalid")

    def test_payload_size_encoding_and_strict_json_guards(self):
        too_large = run_verifier(
            {"schema": {"type": "string"}, "max_payload_bytes": 4},
            b'"abcdef"',
        )
        self.assertFalse(too_large["passed"])
        self.assertEqual(too_large["details"]["reason"], "payload_too_large")

        bad_utf8 = run_verifier({"schema": {"type": "object"}}, b"\xff\xfe")
        self.assertFalse(bad_utf8["passed"])
        self.assertEqual(bad_utf8["details"]["reason"], "payload_not_utf8")

        nan_payload = run_verifier({"schema": {"type": "number"}}, b"NaN")
        self.assertFalse(nan_payload["passed"])
        self.assertEqual(nan_payload["details"]["reason"], "payload_json_invalid")

        dup_req = run_verifier(
            None,
            None,
            raw_request='{"params":{"schema":{}},"params":{"schema":{}},"payload_path":"/tmp/x"}',
        )
        self.assertFalse(dup_req["passed"])
        self.assertEqual(dup_req["details"]["reason"], "request_json_invalid")

    def test_symlink_payload_is_refused(self):
        with tempfile.TemporaryDirectory() as tmpdir:
            real_file = pathlib.Path(tmpdir) / "real.json"
            real_file.write_text("{}", encoding="utf-8")
            link_file = pathlib.Path(tmpdir) / "link.json"
            link_file.symlink_to(real_file)
            req = json.dumps(
                {
                    "params": {"schema": {"type": "object"}},
                    "payload_path": str(link_file),
                }
            )
            verdict = run_verifier(None, None, raw_request=req)
            self.assertFalse(verdict["passed"])
            self.assertEqual(verdict["details"]["reason"], "payload_unavailable")


if __name__ == "__main__":
    unittest.main()
