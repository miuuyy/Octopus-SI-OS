import copy
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("memory_experience", Path(__file__).parents[1] / "verify.py")
verifier = importlib.util.module_from_spec(spec)
spec.loader.exec_module(verifier)


class TrimmingEvidenceTests(unittest.TestCase):
    def setUp(self):
        ref = {"id": "source", "content": [0] * 32}
        self.value = {
            "schema_version": 1, "label": "memory.experience.input", "text": "Experience",
            "source": {"run": "run", "worker_interface": "worker", "provider_interface": "provider",
                       "client_interface": "client", "ee": "ee", "terminal_artifact": ref,
                       "terminal_source_movement_seq": 1, "projection_source_movement_seq": 2},
            "selected_input": {"source_artifact": ref, "source_movement_seq": 0,
                               "provenance": {"source_artifact": ref, "source_movement_seq": 0},
                               "fields": {"/text": "short"}, "original_fields": {"/text": "short"}},
            "terminal_fields": {"/text": "short"}, "evidence": [{"fields": {"/text": "short"}}],
            "omission": {"selected_fields_trimmed": False, "selected_evidence_trimmed": False},
        }

    def check(self, value):
        with tempfile.TemporaryDirectory() as root:
            path = Path(root) / "experience.json"
            path.write_text(json.dumps(value))
            verifier.verify({"payload_path": str(path), "artifact_label": "memory.experience.input",
                             "params": {"stage": "projection", "max_bytes": 131072, "source_run": "run",
                                        "source_worker_interface": "worker", "source_provider_interface": "provider",
                                        "source_client_interface": "client"}})

    def test_source_worker_is_observed_per_event(self):
        self.value["source"]["worker_interface"] = "another_worker"
        self.check(self.value)
        self.value["source"]["worker_interface"] = ""
        with self.assertRaisesRegex(AssertionError, "observed worker"):
            self.check(self.value)

    def test_all_retained_field_locations_are_checked(self):
        for location, key in [("input", "trimmed_fields"), ("input", "original_trimmed_fields"),
                              ("terminal", "terminal_trimmed_fields"), ("evidence", "trimmed_fields")]:
            with self.subTest(location=location, key=key):
                value = copy.deepcopy(self.value)
                target = value["selected_input"] if location == "input" else value if location == "terminal" else value["evidence"][0]
                target[key] = [{"pointer": "/text", "bytes": 9000}]
                value["omission"]["selected_fields_trimmed"] = True
                self.check(value)
                value["omission"]["selected_fields_trimmed"] = False
                with self.assertRaisesRegex(AssertionError, "field trimming is not recorded"):
                    self.check(value)
                value["omission"]["selected_fields_trimmed"] = True
                target[key][0]["pointer"] = "/absent"
                with self.assertRaisesRegex(AssertionError, "trimmed field record malformed"):
                    self.check(value)

    def test_removed_evidence_has_no_remaining_field_trim(self):
        self.value["evidence"] = []
        self.value["omission"].update(selected_evidence_trimmed=True, trimmed_evidence_entries=421)
        self.check(self.value)
        self.value["omission"]["selected_fields_trimmed"] = True
        with self.assertRaisesRegex(AssertionError, "field trimming is not recorded"):
            self.check(self.value)


if __name__ == "__main__":
    unittest.main()
