import copy
import hashlib
import json
import pathlib
import subprocess
import sys
import tempfile
import unittest


SCRIPT = pathlib.Path(__file__).resolve().parents[1] / "verify.py"
WRITER = "memory.save.selected_observer"


def reference(identity):
    return {"id": identity, "content": list(hashlib.sha256(identity.encode()).digest())}


def evidence(seq, label, payload):
    return {"source_artifact": reference(str(seq)), "source_movement_seq": seq,
            "label": label, "payload": payload}


class CaptureOutcomeVerifierTests(unittest.TestCase):
    def test_every_memory_save_receipt_is_required_for_a_publication(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary)
            digest = hashlib.sha256(b"entry").hexdigest()
            path = str(root / "memory-note.json")
            call = evidence(3, "tool.call", {"tool": WRITER, "correlation_id": "save-1", "arguments": {"note": "Prefers pnpm."}})
            result = evidence(4, WRITER + ".result", {"label": WRITER + ".result", "tool": WRITER,
                "correlation_id": "save-1", "ok": True, "error": None,
                "body": {"result": "memory_saved", "memory": {"id": "memory-note", "scope_ee_ids": ["selected-ee"]},
                    "withdrawn": [], "writes": [{"path": path, "outcome": "applied", "error": None,
                        "before_hash": None, "after_hash": digest, "observed_hash": digest, "bytes": 5}]}})
            repeat = evidence(5, "tool.call", {"tool": WRITER, "correlation_id": "save-2", "arguments": {"note": "Prefers pnpm."}})
            deduped = evidence(6, WRITER + ".result", {"label": WRITER + ".result", "tool": WRITER,
                "correlation_id": "save-2", "ok": True, "error": None,
                "body": {"result": "memory_already_saved", "memory": {"id": "memory-note"}}})
            payload = {"schema_version": 1, "label": "memory.capture.outcome",
                "source": {"run": "source", "ee": "selected-ee", "provider_interface": "provider",
                    "terminal_artifact": reference("terminal"), "terminal_source_movement_seq": 20},
                "experience": {"source_artifact": reference("experience"), "source_movement_seq": 1},
                "omission": {"before_current_experience": True, "selected_evidence_trimmed": False},
                "outcome": {"schema_version": 1, "status": "noop", "source_run": "source",
                    "reason": "No additional reusable knowledge.", "receipts": []}, "evidence": []}
            request = {"params": {"max_bytes": 65536, "source_run": "source", "source_ee": "selected-ee",
                "provider_interface": "provider", "publication_tool": WRITER, "entries_root": str(root)},
                "artifact_label": "memory.capture.outcome", "payload_path": str(root / "outcome.json")}

            def check(entries, status, passed, receipts=("save-1",)):
                candidate = copy.deepcopy(payload)
                candidate["evidence"] = entries
                candidate["outcome"]["status"] = status
                if status == "published":
                    candidate["outcome"]["receipts"] = [{"tool": WRITER, "correlation_id": item} for item in receipts]
                pathlib.Path(request["payload_path"]).write_text(json.dumps(candidate))
                completed = subprocess.run([sys.executable, str(SCRIPT)], input=json.dumps(request),
                    text=True, capture_output=True, check=True, timeout=10)
                verdict = json.loads(completed.stdout)
                self.assertEqual(verdict["passed"], passed, verdict)

            invalid = copy.deepcopy(payload)
            invalid.update(outcome=None, outcome_parse_error="outcome is not a JSON value")
            pathlib.Path(request["payload_path"]).write_text(json.dumps(invalid))
            refused = subprocess.run([sys.executable, str(SCRIPT)], input=json.dumps(request), text=True, capture_output=True, check=True)
            verdict = json.loads(refused.stdout)
            self.assertFalse(verdict["passed"])
            self.assertEqual(verdict["details"]["error"], invalid["outcome_parse_error"])
            check([], "noop", True)
            check([repeat, deduped], "noop", True)
            check([call, result], "published", True)
            check([call, result, repeat, deduped], "published", True)
            check([call, result, repeat, deduped], "published", False, ("save-1", "save-2"))
            check([call, result], "noop", False)
            refused_call = evidence(2, "tool.call", {"tool": WRITER, "correlation_id": "save-0", "arguments": {"note": "Prefers pnpm."}})
            for code, passed in (("tool_process_failure", True), ("tool_effect_indeterminate", False)):
                attempt = evidence(3, WRITER + ".result", {"label": WRITER + ".result", "tool": WRITER,
                    "correlation_id": "save-0", "ok": False, "body": None, "error": {"code": code}})
                retried = [refused_call, attempt, dict(call, source_movement_seq=4, source_artifact=reference("4")),
                    dict(result, source_movement_seq=5, source_artifact=reference("5"))]
                with self.subTest(failed_attempt=code):
                    check(retried, "published", passed)
                    check(retried[:2], "noop", passed)
            check([refused_call, evidence(3, WRITER + ".result", {"label": WRITER + ".result", "tool": WRITER,
                "correlation_id": "save-0", "ok": False, "body": None, "error": {"code": "tool_process_failure"}})],
                "published", False, ("save-0",))
            for entries in ([call], [result], [repeat]):
                with self.subTest(entries=[item["label"] for item in entries]):
                    check(entries, "published", False)
            for change in ("failed", "correlation", "order", "partial", "scope", "escape", "other"):
                changed = copy.deepcopy(result)
                body = changed["payload"]["body"]
                if change == "failed":
                    changed["payload"].update(ok=False, body=None, error={"code": "tool_execution_failed"})
                elif change == "correlation":
                    changed["payload"]["correlation_id"] = "unmatched"
                elif change == "order":
                    changed["source_movement_seq"] = 2
                elif change == "partial":
                    body["writes"][0]["outcome"] = "uncertain"
                elif change == "scope":
                    body["memory"]["scope_ee_ids"] = ["wider-ee"]
                elif change == "escape":
                    body["writes"][0]["path"] = str(root.parent / "memory-note.json")
                else:
                    body["memory"]["id"] = "another"
                with self.subTest(change=change):
                    check([call, changed], "published", False)

if __name__ == "__main__":
    unittest.main()
