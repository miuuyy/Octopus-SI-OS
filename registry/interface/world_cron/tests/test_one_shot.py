import importlib.util
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

OWNER = Path(__file__).resolve().parents[1]
DUE = "2026-10-04T12:00:00.123456789Z"


class OneShotContract(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.var = self.root / "private"
        self.ledger = self.root / "movements.jsonl"
        self.runs = self.root / "runs"
        self.runs.mkdir()
        self.document = json.loads((OWNER / "activation.json").read_text())
        self.document["activations"][0]["enabled"] = True
        self.config = self.document["activations"][0]["config"]
        self.config["schedule"].update(interval_seconds=None, due_at=DUE, fire_on_start=False)
        environment = {
            "OCTOPUS_INTERFACE_ID": "world_cron",
            "OCTOPUS_INTERFACE_DIR": str(self.root),
            "OCTOPUS_VAR_DIR": str(self.var),
            "OCTOPUS_INTERFACE_DESCRIPTOR_JSON": (OWNER / "interface.json").read_text(),
            "OCTOPUS_MOVEMENTS_JSONL": str(self.ledger),
            "OCTOPUS_INTERFACE_HOST_IO_LIMITS": json.dumps({key: 1048576 for key in ("max_control_frame_bytes", "max_manifest_bytes", "max_payload_bytes", "max_stderr_bytes")}),
            "OCTOPUS_ROOT_SLOTS_JSON": json.dumps({"run_records_root": str(self.runs), "timezone_database_root": str(self.root / "timezone_database_root")}),
        }
        self.environment = patch.dict(os.environ, environment)
        self.environment.start()
        self.addCleanup(self.environment.stop)
        self.module = self.reload()
        self.clock = self.module.parse_due_at(DUE) - 2_000_000_000
        self.sleeps = []
        self.emissions = []

    def reload(self):
        spec = importlib.util.spec_from_file_location("cron_test", OWNER / "cron_life.py")
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        return module

    def schedule(self):
        (self.root / "activation.json").write_text(json.dumps(self.document))
        return self.module.load_schedule()

    def sleep(self, duration):
        self.sleeps.append(duration)
        self.clock += round(duration * 1_000_000_000)

    def admit(self, manifest_path, evidence):
        manifest = json.loads(Path(manifest_path).read_text())
        payload = json.loads(Path(manifest["payload_path"]).read_text())
        self.emissions.append(payload)
        movement = {"kind": "InterfaceArrival", "from": {"Interface": "world_cron"}, "artifact": {"id": evidence[0]["artifact_id"]}, "seq": 7}
        self.ledger.write_text(json.dumps(movement) + "\n")
        return 7

    def execute(self, hand=None):
        with patch.object(self.module.time, "time_ns", lambda: self.clock), patch.object(self.module.time, "sleep", self.sleep), patch.object(self.module, "hand_artifact_with_evidence", hand or self.admit):
            return self.module.run_one_shot(self.schedule(), "Check result")

    def test_descriptor_deadline_is_absolute_and_exclusive(self):
        self.assertEqual(self.schedule()["due_unix_ns"], 1791115200123456789)
        self.assertEqual(self.module.parse_due_at("2026-10-04T14:00:00.123456789+02:00"), self.module.parse_due_at(DUE))
        for value in (True, "2026-10-04T12:00:00", "2026-02-30T00:00:00Z", "2026-10-04T12:00:00-00:00", "2026-10-04T12:00:00+25:00"):
            with self.subTest(value=value), self.assertRaises(ValueError):
                self.module.parse_due_at(value)
        self.config["schedule"]["fire_on_start"] = True
        with self.assertRaisesRegex(ValueError, "fire_on_start=false"):
            self.schedule()
        self.config["schedule"]["fire_on_start"] = False
        self.config["schedule"]["interval_seconds"] = 60
        with self.assertRaisesRegex(ValueError, "exactly one"):
            self.schedule()
        del self.config["schedule"]["due_at"]
        self.assertEqual(self.schedule()["interval_ns"], 60_000_000_000)
        self.config["schedule"].update(interval_seconds=None, cron="*/5 * * * *")
        self.assertEqual(self.schedule()["cron"], "*/5 * * * *")

    def test_disabled_activation_waits_without_emitting_and_observes_enable(self):
        self.document["activations"][0]["enabled"] = False
        self.assertIsNone(self.schedule())
        with patch.object(self.module, "signal_ready") as ready, patch.object(self.module.time, "sleep", side_effect=InterruptedError), patch.object(self.module, "run_one_shot") as emit:
            with self.assertRaises(InterruptedError):
                self.module.main()
        ready.assert_called_once()
        emit.assert_not_called()
        self.document["activations"][0]["enabled"] = True
        self.assertIsNotNone(self.schedule())

    def test_waits_once_and_restart_does_not_reemit_or_rearm(self):
        self.assertEqual(self.execute(), 7)
        self.assertEqual(self.sleeps, [2.0])
        self.assertEqual(len(self.emissions), 1)
        self.assertEqual(self.emissions[0]["schedule_kind"], "one_shot")
        self.assertEqual(self.emissions[0]["due_at"], DUE)
        self.assertEqual(self.emissions[0]["scheduled_unix_ns"], self.module.parse_due_at(DUE))
        self.assertEqual(self.module.load_one_shot_state(self.schedule())["due_unix_ns"], "1791115200123456789")
        later_edge = json.loads(self.ledger.read_text())
        later_edge["seq"] = 8
        with self.ledger.open("a") as ledger:
            ledger.write(json.dumps(later_edge) + "\n")
        self.module = self.reload()
        self.clock += 10**15
        self.assertEqual(self.execute(), 7)
        self.assertEqual(len(self.emissions), 1)
        self.ledger.write_text("")
        with self.assertRaisesRegex(RuntimeError, "recorded arrival evidence"):
            self.execute()

    def test_crash_after_admission_reuses_prepared_payload_and_exact_ledger_evidence(self):
        def lost_ack(manifest, evidence):
            self.admit(manifest, evidence)
            raise RuntimeError("connection lost before acknowledgement")
        with self.assertRaisesRegex(RuntimeError, "connection lost"):
            self.execute(lost_ack)
        state = self.module.load_one_shot_state(self.schedule())
        self.assertEqual(state["status"], "prepared")
        self.module = self.reload()
        self.assertEqual(self.execute(), 7)
        self.assertEqual(len(self.emissions), 1)
        self.config["schedule"]["reminder_default"] = "Changed after preparation"
        with self.assertRaisesRegex(ValueError, "configuration changed"):
            self.execute()

    def test_refusal_does_not_record_success(self):
        def refused(*_):
            raise self.module.DeliveryRefused({"refused": {"reason": "policy", "detail": {}}})
        with self.assertRaises(self.module.DeliveryRefused):
            self.execute(refused)
        self.assertEqual(self.module.load_one_shot_state(self.schedule())["status"], "prepared")
        self.assertEqual(self.emissions, [])


if __name__ == "__main__":
    unittest.main()
