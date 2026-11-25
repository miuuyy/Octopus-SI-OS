#!/usr/bin/env python3
import json
import os
import pathlib
import subprocess
import sys
import tempfile
import unittest


SCRIPT = pathlib.Path(__file__).resolve().parents[1] / "verify.py"


def run_verifier(params, payload_text, raw_request=None):
    with tempfile.TemporaryDirectory() as tmpdir:
        payload_path = pathlib.Path(tmpdir) / "payload"
        if payload_text is not None:
            payload_path.write_text(payload_text, encoding="utf-8")
        if raw_request is None:
            request_text = json.dumps(
                {
                    "run": "run_test",
                    "checkpoint": "cp_test",
                    "epoch": 0,
                    "mark": "diff.scope.allowed",
                    "artifact": {"id": "art_diff", "content": "0" * 64},
                    "artifact_label": "patch.diff",
                    "params": params,
                    "metadata": {},
                    "payload_path": str(payload_path) if payload_text is not None else None,
                    "payload_utf8": payload_text,
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


class DiffScopeVerifierTests(unittest.TestCase):
    def test_exact_file_prefix_and_standalone_unified_diff(self):
        diff = "--- README.md\t2026-01-01\n+++ README.md\t2026-01-02\n@@ -1 +1 @@\n-old\n+new\n"
        ok = run_verifier({"allowed_prefixes": ["README.md"], "timeout_ms": 1000}, diff)
        self.assertTrue(ok["passed"], ok)
        self.assertEqual(ok["details"]["touched_paths"], ["README.md"])

        sibling = "--- README.md.bak\n+++ README.md.bak\n@@ -1 +1 @@\n-old\n+new\n"
        refused = run_verifier({"allowed_prefixes": ["README.md"]}, sibling)
        self.assertFalse(refused["passed"])
        self.assertEqual(refused["details"]["reason"], "path_out_of_scope")

    def test_rename_checks_both_source_and_destination_and_spoofing(self):
        good_rename = (
            "diff --git a/docs/old.md b/docs/new.md\n"
            "similarity index 100%\n"
            "rename from docs/old.md\n"
            "rename to docs/new.md\n"
        )
        ok = run_verifier({"allowed_prefixes": ["docs/"]}, good_rename)
        self.assertTrue(ok["passed"], ok)
        self.assertEqual(ok["details"]["touched_paths"], ["docs/old.md", "docs/new.md"])

        escape_rename = (
            "diff --git a/docs/old.md b/crates/escape.rs\n"
            "rename from docs/old.md\n"
            "rename to crates/escape.rs\n"
        )
        refused = run_verifier({"allowed_prefixes": ["docs/"]}, escape_rename)
        self.assertFalse(refused["passed"])
        self.assertEqual(refused["details"]["reason"], "path_out_of_scope")

        spoofed_rename = (
            "diff --git a/docs/old.md b/docs/new.md\n"
            "rename from docs/old.md\n"
            "rename to crates/sneaky.rs\n"
        )
        mismatch = run_verifier({"allowed_prefixes": ["docs/"]}, spoofed_rename)
        self.assertFalse(mismatch["passed"])
        self.assertEqual(mismatch["details"]["reason"], "diff_header_mismatch")

    def test_hunk_lines_starting_with_triple_plus_cannot_spoof_header(self):
        diff = (
            "diff --git a/docs/a.md b/docs/a.md\n"
            "--- a/docs/a.md\n"
            "+++ b/docs/a.md\n"
            "@@ -1,1 +1,2 @@\n"
            " keep\n"
            "+++ b/not_a_header.md\n"
        )
        ok = run_verifier({"allowed_prefixes": ["docs/"]}, diff)
        self.assertTrue(ok["passed"], ok)
        self.assertEqual(ok["details"]["added_lines"], 1)
        self.assertEqual(ok["details"]["touched_paths"], ["docs/a.md"])

    def test_secret_patterns_detected_on_added_lines_and_never_echoed(self):
        tokens = [
            ("github_pat", "ghp_" + "A" * 24),
            ("github_fine_grained_pat", "github_pat_" + "B" * 24),
            ("aws_access_key_id", "AKIA" + "0" * 16),
            ("api_secret_key", "sk-" + "live-" + "C" * 20),
            ("private_key_pem", "-----BEGIN " + "EC PRIVATE KEY-----"),
        ]
        for rule_name, token in tokens:
            with self.subTest(rule=rule_name):
                added_diff = (
                    "diff --git a/docs/a.md b/docs/a.md\n"
                    "--- a/docs/a.md\n"
                    "+++ b/docs/a.md\n"
                    "@@ -1 +1,2 @@\n"
                    " line\n"
                    f"+value = {token}\n"
                )
                verdict = run_verifier({"allowed_prefixes": ["docs/"]}, added_diff)
                self.assertFalse(verdict["passed"])
                self.assertEqual(verdict["details"]["reason"], "secret_pattern_detected")
                self.assertEqual(verdict["details"]["rule"], rule_name)
                self.assertNotIn(token, json.dumps(verdict))

                removed_diff = (
                    "diff --git a/docs/a.md b/docs/a.md\n"
                    "--- a/docs/a.md\n"
                    "+++ b/docs/a.md\n"
                    "@@ -1,2 +1 @@\n"
                    " line\n"
                    f"-value = {token}\n"
                )
                removed_ok = run_verifier({"allowed_prefixes": ["docs/"]}, removed_diff)
                self.assertTrue(removed_ok["passed"], removed_ok)

    def test_limits_empty_binary_and_unsafe_paths(self):
        ok_empty = run_verifier({"allowed_prefixes": ["docs/"], "allow_empty": True}, "")
        self.assertTrue(ok_empty["passed"])
        self.assertEqual(ok_empty["details"]["file_count"], 0)

        ok_binary = run_verifier(
            {"allowed_prefixes": ["docs/"], "allow_binary": True},
            "diff --git a/docs/a.bin b/docs/a.bin\nBinary files a/docs/a.bin and b/docs/a.bin differ\n",
        )
        self.assertTrue(ok_binary["passed"], ok_binary)

        quoted_diff = 'diff --git "a/docs/a\\nb.md" "b/docs/a\\nb.md"\n'
        quoted = run_verifier({"allowed_prefixes": ["docs/"]}, quoted_diff)
        self.assertFalse(quoted["passed"])
        self.assertEqual(quoted["details"]["reason"], "unsafe_path")

        too_many_lines = run_verifier(
            {"allowed_prefixes": ["docs/"], "max_changed_lines": 1},
            "diff --git a/docs/a.md b/docs/a.md\n--- a/docs/a.md\n+++ b/docs/a.md\n@@ -1 +1 @@\n-old\n+new\n",
        )
        self.assertFalse(too_many_lines["passed"])
        self.assertEqual(too_many_lines["details"]["reason"], "max_changed_lines_exceeded")


if __name__ == "__main__":
    unittest.main()
