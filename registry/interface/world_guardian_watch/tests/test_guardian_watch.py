import importlib.util
import json
import os
from pathlib import Path
import re
import tempfile
import unittest
from unittest.mock import patch

HERE = Path(__file__).resolve().parents[1]
LIMITS = dict(max_control_frame_bytes=1048576, max_manifest_bytes=1048576,
              max_payload_bytes=8388608, max_stderr_bytes=262144)
RUN, PROVIDER, SENSOR = "run_centre", "provider_centre", "world_guardian_watch"
HOUR_MS = 3600 * 1000


def config(**changes):
    value = dict(schema_version=1, watched_run=RUN, provider_interface=PROVIDER, quiet_seconds=3600,
                 repeat_cooldown_seconds=3600, max_repeat_cooldown_seconds=21600, refused_retry_seconds=600,
                 poll_seconds=15, own_action_labels=["tool.call", "turn.agent_message"],
                 error_labels=["turn.error"], heartbeat_labels=["cron.current_time"],
                 notice_label="guardian.quiet_alert", instructions="Look at the run.")
    value.update(changes)
    return value


class Case(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.var = self.root / "var"
        self.ledger = self.root / "movements.jsonl"
        env = dict(OCTOPUS_INTERFACE_ID=SENSOR, OCTOPUS_INTERFACE_DIR=str(HERE), OCTOPUS_VAR_DIR=str(self.var),
                   OCTOPUS_MOVEMENTS_JSONL=str(self.ledger), OCTOPUS_INTERFACE_HOST_IO_LIMITS=json.dumps(LIMITS))
        with patch.dict(os.environ, env):
            spec = importlib.util.spec_from_file_location("guardian_watch_life_under_test", HERE / "guardian_watch_life.py")
            self.g = importlib.util.module_from_spec(spec)
            spec.loader.exec_module(self.g)
        self.logs = []
        quiet = patch.object(self.g, "log", self.logs.append)
        quiet.start()
        self.addCleanup(quiet.stop)
        self.seq = -1

    def row(self, label, source=PROVIDER, target="worker", kind="InterfaceArrival", run=RUN, at=1000):
        self.seq += 1
        return {"seq": self.seq, "recorded_at_unix_ms": at, "kind": kind, "from": {"Interface": source},
                "to": {"Interface": target}, "artifact": {"id": label + ":" + "0" * 64},
                "invocation_metadata": {"runtime": {"run": run}}}

    def append(self, *rows, tail=""):
        with self.ledger.open("a") as handle:
            for row in rows:
                handle.write(json.dumps(row) + "\n")
            handle.write(tail)

    def state(self, cfg=None):
        return self.g.empty_state(cfg or config())


class Configuration(Case):
    def registry(self, value):
        path = self.root / "registry_guardian_config.json"
        path.write_text(json.dumps(value))
        replaced = patch.object(self.g, "REGISTRY_CONFIG_PATH", str(path))
        replaced.start()
        self.addCleanup(replaced.stop)
        return path

    def test_the_packaged_template_names_no_installation_and_is_refused(self):
        text = (HERE / "guardian_config.json").read_text()
        template = json.loads(text)
        self.assertEqual((template["watched_run"], template["provider_interface"]), ("RUN_ID", "PROVIDER_INTERFACE_ID"))
        self.assertIsNone(re.search(r"/Users/|/home/|run_operator|[0-9a-f]{16,}|\d+\.\d+\.\d+\.\d+", text))
        with self.assertRaisesRegex(ValueError, "still names the template provider_interface"):
            self.g.load_config()
        self.registry(config(provider_interface="PROVIDER_INTERFACE_ID"))
        with self.assertRaisesRegex(ValueError, "still names the template provider_interface"):
            self.g.load_config()
        self.registry(config(watched_run="RUN_ID"))
        with self.assertRaisesRegex(ValueError, "still names the template watched_run"):
            self.g.load_config()

    def test_private_state_config_overrides_the_registry_folder_and_fails_closed(self):
        registry = self.registry(config(watched_run="run_from_registry"))
        loaded, stamp = self.g.load_config()
        self.assertEqual((loaded["watched_run"], stamp[0]), ("run_from_registry", str(registry)))
        self.var.mkdir()
        private = self.var / "guardian_config.json"
        private.write_text(json.dumps(config(watched_run="run_from_private_state", quiet_seconds=5400)))
        self.assertNotEqual(self.g.config_stamp(), stamp)
        loaded, stamp = self.g.load_config()
        self.assertEqual((loaded["watched_run"], loaded["quiet_seconds"], stamp[0]),
                         ("run_from_private_state", 5400, str(private)))
        # An unusable private file is an error, never a fallback to the registry folder.
        private.write_text('{"schema_version": 1,')
        with self.assertRaises(ValueError):
            self.g.load_config()
        private.write_text(json.dumps(dict(config(), surprise=True)))
        with self.assertRaisesRegex(ValueError, "keys are wrong"):
            self.g.load_config()
        private.write_text(json.dumps(config(own_action_labels=[])))
        with self.assertRaisesRegex(ValueError, "own_action_labels must not be empty"):
            self.g.load_config()
        private.write_text(json.dumps(config(max_repeat_cooldown_seconds=60)))
        with self.assertRaisesRegex(ValueError, "below repeat_cooldown_seconds"):
            self.g.load_config()
        private.unlink()
        private.symlink_to(registry)
        with self.assertRaises(PermissionError):
            self.g.load_config()
        private.unlink()
        self.assertEqual(self.g.load_config()[0]["watched_run"], "run_from_registry")


class Fold(Case):
    def test_only_the_providers_own_emissions_count_as_life(self):
        cfg, state = config(), self.state()
        own = self.row("tool.call", at=1000)
        for row in (
            own,
            self.row("file.write__room.result", source="room", target=PROVIDER, kind="InterfaceOutbound", at=2000),
            self.row("cron.current_time", source="world_cron", target=PROVIDER, kind="InterfaceOutbound", at=3000),
            self.row("provider.peer.input", source="service", target=PROVIDER, kind="InterfaceArrival", at=3500),
            self.row("turn.token_usage", at=4000),
            self.row("turn.error", at=5000),
            self.row("tool.call", run="another_run", at=6000),
            self.row("tool.call", source="another_provider", at=7000),
            dict(self.row("tool.call", at=8000), kind="DeliveryRefused"),
        ):
            self.g.observe(state, cfg, row)
        self.assertEqual(state["last_own_action"], {"seq": own["seq"], "kind": "InterfaceArrival", "label": "tool.call",
                                                    "recorded_at_unix_ms": 1000})
        self.assertEqual((state["last_provider_emission"]["label"], state["last_provider_error"]["label"]),
                         ("turn.error", "turn.error"))
        self.assertEqual((state["inbound_since"]["inputs"], state["inbound_since"]["tool_results"]), (1, 1))
        self.assertEqual([entry["from"] for entry in state["inbound_since"]["recent"]], ["room", "world_cron"])
        self.assertEqual(state["last_heartbeat"]["label"], "cron.current_time")
        self.assertEqual(state["ledger_tail"], {"seq": self.seq, "recorded_at_unix_ms": 8000})
        # Deliveries never postpone the alert: the quiet period is still measured from the own action.
        self.assertEqual(self.g.evaluate(state, cfg, 1000 + HOUR_MS + 1)["for_seq"], own["seq"])
        self.g.observe(state, cfg, self.row("turn.agent_message", at=9000))
        self.assertEqual(state["inbound_since"], {"inputs": 0, "tool_results": 0, "recent": []})
        self.assertIsNone(self.g.evaluate(state, cfg, 1000 + HOUR_MS + 1))

    def test_scan_resumes_at_its_cursor_and_folds_a_replaced_ledger_again(self):
        cfg, state = config(), self.state()
        self.g.scan(state, cfg)
        self.assertEqual(state["cursor"]["next_seq"], 0)
        first = self.row("tool.call", at=1000)
        partial = json.dumps(self.row("turn.agent_message", at=3000))
        self.append(first, tail="not json\n" + partial[:20])
        self.g.scan(state, cfg)
        self.assertEqual((state["cursor"]["next_seq"], state["skipped_rows"], state["last_own_action"]["seq"]), (1, 1, 0))
        offset = state["cursor"]["offset"]
        self.g.scan(state, cfg)
        self.assertEqual(state["cursor"]["offset"], offset)
        with self.ledger.open("a") as handle:
            handle.write(partial[20:] + "\n")
        self.g.scan(state, cfg)
        self.assertEqual((state["cursor"]["next_seq"], state["last_own_action"]["label"]), (2, "turn.agent_message"))
        self.g.save_state(state)
        self.assertEqual(self.g.load_state(cfg), state)
        self.assertIsNone(self.g.load_state(config(watched_run="another_run"))["last_own_action"])
        self.ledger.write_text(json.dumps(dict(self.row("tool.call", at=500), seq=0)) + "\n")
        self.g.scan(state, cfg)
        self.assertEqual((state["cursor"]["next_seq"], state["last_own_action"]["recorded_at_unix_ms"]), (1, 500))
        self.assertTrue(any("shrank" in line for line in self.logs))


class Alerts(Case):
    def test_quiet_threshold_growing_cooldown_and_refused_retry(self):
        cfg, state = config(), self.state()
        started = state["started_at_unix_ms"]
        self.assertIsNone(self.g.evaluate(state, cfg, started + HOUR_MS))
        self.assertEqual(self.g.evaluate(state, cfg, started + HOUR_MS + 1), {"for_seq": -1, "repeat_index": 0, "quiet_ms": HOUR_MS + 1})
        self.g.observe(state, cfg, self.row("tool.call", at=10_000))
        now = 10_000 + HOUR_MS + 1
        self.assertEqual(self.g.evaluate(state, cfg, now)["for_seq"], 0)
        alert = {"status": "prepared", "for_seq": 0, "repeat_index": 0, "at_unix_ms": now}
        state["alert"] = alert
        self.assertIsNone(self.g.evaluate(state, cfg, now + 100 * HOUR_MS))
        alert["status"] = "admitted"
        self.assertIsNone(self.g.evaluate(state, cfg, now + HOUR_MS - 1))
        self.assertEqual(self.g.evaluate(state, cfg, now + HOUR_MS)["repeat_index"], 1)
        self.assertEqual([self.g.repeat_wait_seconds(cfg, dict(alert, repeat_index=index)) for index in range(5)],
                         [3600, 7200, 14400, 21600, 21600])
        alert.update(status="refused", repeat_index=2)
        self.assertEqual(self.g.repeat_wait_seconds(cfg, alert), 600)
        self.assertIsNone(self.g.evaluate(state, cfg, now + 599_999))
        self.assertEqual(self.g.evaluate(state, cfg, now + 600_000)["repeat_index"], 2)
        # A new own action ends the reported silence; the next one starts its own count.
        self.g.observe(state, cfg, self.row("tool.call", at=now))
        self.assertIsNone(self.g.evaluate(state, cfg, now + HOUR_MS))
        self.assertEqual(self.g.evaluate(state, cfg, now + HOUR_MS + 1), {"for_seq": 1, "repeat_index": 0, "quiet_ms": HOUR_MS + 1})

    def prepared(self, cfg):
        state = self.state(cfg)
        self.append(self.row("tool.call", at=10_000),
                    self.row("cron.current_time", source="world_cron", target=PROVIDER, kind="InterfaceOutbound", at=20_000))
        self.g.scan(state, cfg)
        now = 10_000 + 2 * HOUR_MS
        self.g.prepare_alert(state, cfg, self.g.evaluate(state, cfg, now), now)
        return state

    def admit(self, manifest_path, evidence):
        self.assertEqual(evidence[0]["label"], "guardian_watch.arrival.idempotency")
        self.append({"seq": 900, "kind": "InterfaceArrival", "from": {"Interface": SENSOR},
                     "artifact": {"id": evidence[0]["artifact_id"]}})
        return 900

    def test_an_alert_is_prepared_before_hand_in_and_admitted_once(self):
        cfg = config()
        state = self.prepared(cfg)
        alert = state["alert"]
        self.assertEqual((alert["status"], alert["for_seq"], alert["repeat_index"], alert["admitted_seq"]), ("prepared", 0, 0, None))
        self.assertEqual(json.loads((self.var / "guardian-state.json").read_text())["alert"]["status"], "prepared")
        manifest = json.loads(Path(alert["manifest_path"]).read_bytes())
        self.assertEqual(self.g.octopus_content_hash(Path(alert["manifest_path"]).read_bytes()), alert["manifest_hash"])
        payload_bytes = Path(manifest["payload_path"]).read_bytes()
        self.assertEqual((manifest["label"], manifest["payload_content_hash"]), ("guardian.quiet_alert", self.g.octopus_content_hash(payload_bytes)))
        payload = json.loads(payload_bytes)
        self.assertEqual((payload["trigger"]["watched_run"], payload["trigger"]["provider_interface"], payload["quiet_seconds"], payload["task"]),
                         (RUN, PROVIDER, 7200, "Look at the run."))
        self.assertIn("Deliveries to it do not count", payload["trigger"]["event"])
        self.assertEqual((payload["last_own_action"]["seq"], payload["heartbeat_delivered_since_last_own_action"]), (0, True))
        self.assertEqual(payload["alert"], {"number": 1, "repeat_index": 0, "next_report_of_this_silence_after_seconds": 3600})
        self.assertEqual(payload["run_record"]["readable"], False)
        with patch.object(self.g, "hand", side_effect=self.admit) as hand:
            self.g.deliver(state)
        self.assertEqual((hand.call_count, state["alert"]["status"], state["alert"]["admitted_seq"], state["alerts_sent"]), (1, "admitted", 900, 1))
        # A restart with the same prepared alert finds the arrival in the ledger and hands nothing again.
        state["alert"].update(status="prepared", admitted_seq=None)
        with patch.object(self.g, "hand", side_effect=AssertionError("handed again")):
            self.g.deliver(state)
        self.assertEqual((state["alert"]["admitted_seq"], state["alerts_sent"]), (900, 2))

    def test_the_run_record_is_described_only_when_its_root_is_granted(self):
        runs = self.root / "runs"
        runs.mkdir()
        (runs / (RUN + ".json")).write_text(json.dumps({"id": RUN, "ee": "ee_centre", "status": "idle",
                                                        "checkpoints": [{}, {}], "active_checkpoints": ["a"]}))
        with patch.dict(os.environ, {"OCTOPUS_ROOT_SLOTS_JSON": json.dumps({"run_records_root": str(runs)})}):
            snapshot = self.g.run_record_snapshot(config())
            missing = self.g.run_record_snapshot(config(watched_run="absent_run"))
        self.assertEqual((snapshot["readable"], snapshot["status"], snapshot["ee"], snapshot["checkpoints"], snapshot["active_checkpoints"]),
                         (True, "idle", "ee_centre", 2, 1))
        self.assertEqual(missing["readable"], False)
        with patch.dict(os.environ, {"OCTOPUS_ROOT_SLOTS_JSON": "{}"}):
            self.assertEqual(self.g.run_record_snapshot(config())["reason"], "run_records_root is not granted to this sensor")

    def test_refusals_are_recorded_and_duplicates_need_ledger_evidence(self):
        cfg = config()
        state = self.prepared(cfg)
        refused = self.g.Refused({"refused": {"reason": "no_binding", "detail": {"interface": SENSOR}}})
        with patch.object(self.g, "hand", side_effect=refused):
            self.g.deliver(state)
        self.assertEqual((state["alert"]["status"], state["alert"]["refusal"]["reason"], state["alerts_sent"]), ("refused", "no_binding", 0))
        self.assertEqual(json.loads((self.var / "guardian-state.json").read_text())["alert"]["status"], "refused")

        state = self.prepared(cfg)
        artifact_id = state["alert"]["label"] + ":" + state["alert"]["manifest_hash"]
        duplicate = self.g.Refused({"refused": {"reason": "policy", "detail": {"artifact_id": artifact_id, "message": self.g.DUPLICATE_MESSAGE}}})
        with patch.object(self.g, "hand", side_effect=duplicate):
            with self.assertRaisesRegex(RuntimeError, "the ledger has no such arrival"):
                self.g.deliver(state)

        def admitted_then_duplicate(manifest_path, evidence):
            self.admit(manifest_path, evidence)
            raise duplicate
        with patch.object(self.g, "hand", side_effect=admitted_then_duplicate):
            self.g.deliver(state)
        self.assertEqual((state["alert"]["status"], state["alert"]["admitted_seq"]), ("admitted", 900))

    def test_a_transient_refusal_is_handed_again_without_a_new_alert(self):
        cfg = config()
        state = self.prepared(cfg)
        manifest_hash = state["alert"]["manifest_hash"]
        transient = self.g.Refused({"refused": {"reason": "registry_unstable", "detail": {}}})
        with patch.object(self.g, "hand", side_effect=[transient, transient, 41]) as hand, patch.object(self.g.time, "sleep") as sleep:
            self.g.deliver(state)
        self.assertEqual((hand.call_count, [call.args[0] for call in sleep.call_args_list]), (3, [1, 2]))
        self.assertEqual((state["alert"]["status"], state["alert"]["admitted_seq"], state["alert"]["manifest_hash"]), ("admitted", 41, manifest_hash))


if __name__ == "__main__":
    unittest.main()
