#!/usr/bin/env python3
import hashlib
import json
import os
import pathlib
import subprocess
import sys
import tempfile
import unittest


SCRIPT = pathlib.Path(__file__).resolve().parents[1] / "verify.py"


def run_verifier(params, payload_bytes, artifact_content_hex=None, raw_request=None):
    with tempfile.TemporaryDirectory() as tmpdir:
        payload_path = pathlib.Path(tmpdir) / "payload"
        if payload_bytes is not None:
            payload_path.write_bytes(payload_bytes)
        if raw_request is None:
            request_text = json.dumps(
                {
                    "run": "run_test",
                    "checkpoint": "cp_test",
                    "epoch": 0,
                    "mark": "artifact.digest.verified",
                    "artifact": {
                        "id": "art_bin",
                        "content": artifact_content_hex if artifact_content_hex is not None else "0" * 64,
                    },
                    "artifact_label": "bundle.tar.gz",
                    "params": params,
                    "metadata": {},
                    "payload_path": str(payload_path) if payload_bytes is not None else None,
                    "payload_utf8": None,
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


class ArtifactDigestVerifierTests(unittest.TestCase):
    def test_official_blake3_vectors_across_chunk_and_stream_boundaries(self):
        # Official BLAKE3 test vectors for bytes(i % 251 for i in range(n)).
        vectors = [
            (b"", "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262"),
            (b"abc", "6437b3ac38465133ffb63b75273a8db548c558465d79db03fd359c6cd5bd9d85"),
            (
                bytes(i % 251 for i in range(1024)),
                "42214739f095a406f3fc83deb889744ac00df831c10daa55189b5d121c855af7",
            ),
            (
                bytes(i % 251 for i in range(1025)),
                "d00278ae47eb27b34faecf67b4fe263f82d5412916c1ffd97c8cb7fb814b8444",
            ),
            (
                bytes(i % 251 for i in range(2048)),
                "e776b6028c7cd22a4d0ba182a8bf62205d2ef576467e838ed6f2529b85fba24a",
            ),
        ]
        for payload, expected_b3 in vectors:
            with self.subTest(length=len(payload)):
                expected_sha = hashlib.sha256(payload).hexdigest()
                verdict = run_verifier(
                    {
                        "sha256": expected_sha,
                        "blake3": expected_b3,
                        "match_artifact_content_hash": True,
                        "exact_bytes": len(payload),
                        "timeout_ms": 2000,
                    },
                    payload,
                    artifact_content_hex=expected_b3,
                )
                self.assertTrue(verdict["passed"], verdict)
                self.assertEqual(verdict["details"]["byte_length"], len(payload))
                self.assertEqual(verdict["details"]["sha256"], expected_sha)
                self.assertEqual(verdict["details"]["blake3"], expected_b3)

    def test_streaming_above_64k_chunk_size(self):
        payload = bytes((i * 31) & 0xFF for i in range(70000))
        expected_sha = hashlib.sha256(payload).hexdigest()
        verdict = run_verifier(
            {"sha256": expected_sha, "min_bytes": 65537, "max_bytes": 80000},
            payload,
        )
        self.assertTrue(verdict["passed"], verdict)
        self.assertEqual(verdict["details"]["byte_length"], 70000)
        self.assertNotIn("blake3", verdict["details"])

    def test_incompatible_bounds_and_boolean_params_fail_closed(self):
        cases = [
            {"min_bytes": 10, "max_bytes": 5},
            {"min_bytes": 10, "exact_bytes": 5},
            {"max_bytes": 5, "exact_bytes": 10},
            {"min_bytes": True},
            {"max_bytes": False},
            {"exact_bytes": -1},
            {"timeout_ms": 0},
            {"timeout_ms": True},
            {"match_artifact_content_hash": "yes"},
        ]
        for params in cases:
            with self.subTest(params=params):
                verdict = run_verifier(params, b"abc")
                self.assertFalse(verdict["passed"])
                self.assertEqual(verdict["details"]["reason"], "params_invalid")

    def test_symlink_payload_is_refused(self):
        with tempfile.TemporaryDirectory() as tmpdir:
            real_file = pathlib.Path(tmpdir) / "real.bin"
            real_file.write_bytes(b"abc")
            link_file = pathlib.Path(tmpdir) / "link.bin"
            link_file.symlink_to(real_file)
            req = json.dumps(
                {
                    "params": {"exact_bytes": 3},
                    "payload_path": str(link_file),
                }
            )
            verdict = run_verifier(None, None, raw_request=req)
            self.assertFalse(verdict["passed"])
            self.assertEqual(verdict["details"]["reason"], "payload_unavailable")

    def test_invalid_artifact_content_hash_fails_closed(self):
        verdict = run_verifier(
            {"match_artifact_content_hash": True},
            b"abc",
            artifact_content_hex="not-hex",
        )
        self.assertFalse(verdict["passed"])
        self.assertEqual(verdict["details"]["reason"], "artifact_content_hash_invalid")


if __name__ == "__main__":
    unittest.main()
