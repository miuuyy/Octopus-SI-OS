import json
import os
from pathlib import Path
import unittest
from unittest.mock import patch

import test_one_shot


class InactivityContract(unittest.TestCase):
    reload = test_one_shot.OneShotContract.reload
    schedule = test_one_shot.OneShotContract.schedule

    def setUp(self):
        test_one_shot.OneShotContract.setUp(self)
        self.config["schedule"].pop("due_at")
        self.config["schedule"]["inactivity"] = {
            "seconds": 3600,
            "interface_match": {"interface_type": "world", "role": "user", "interface_driver_ref": "app_server"},
        }
        slots = json.loads(os.environ["OCTOPUS_ROOT_SLOTS_JSON"])
        slots["registry_interface_root"] = str(self.root / "interfaces")
        os.environ["OCTOPUS_ROOT_SLOTS_JSON"] = json.dumps(slots)
        self.interfaces = self.root / "interfaces"
        for name, role in (("world_user", "user"), ("world_background", "sensor")):
            folder = self.interfaces / name
            folder.mkdir(parents=True)
            (folder / "interface.json").write_text(json.dumps({"id": name, "interface_type": "world", "role": role, "interface_driver_ref": "app_server"}))
        for name, interface in (("run_a", "world_user"), ("run_b", "world_user"), ("run_passive", "world_background"), ("run_review", "world_user")):
            interfaces = [interface] + (["world_cron"] if name == "run_review" else [])
            (self.runs / f"{name}.json").write_text(json.dumps({"id": name, "checkpoints": [{"interface_binding": {"interface": item}} for item in interfaces]}))
        self.clock = 1_800_000_000_000_000_000
        self.seq = -1
        self.patches = [patch.object(self.module.time, "time_ns", lambda: self.clock), patch.object(self.module, "hand_artifact_with_evidence", self.admit)]
        for p in self.patches:
            p.start()
            self.addCleanup(p.stop)

    def movement(self, run, artifact="any.label:123", kind="InterfaceOutbound", source="world_user"):
        self.seq += 1
        row = {"seq": self.seq, "kind": kind, "from": {"Interface": source}, "artifact": {"id": artifact}, "recorded_at_unix_ms": self.clock // 1_000_000, "invocation_metadata": {"runtime": {"run": run}}}
        with self.ledger.open("a") as out:
            out.write(json.dumps(row) + "\n")
        return self.seq

    def admit(self, manifest, evidence):
        payload = Path(json.loads(Path(manifest).read_text())["payload_path"])
        self.emissions.append(json.loads(payload.read_text()))
        return self.movement("run_review", evidence[0]["artifact_id"], "InterfaceArrival", "world_cron")

    def trigger(self):
        return self.module.InactivityTrigger(self.schedule(), "Improve what is useful.")

    def advance(self, seconds):
        self.clock += int(seconds * 1_000_000_000)

    def test_all_user_runs_reset_timer_but_background_and_self_do_not(self):
        trigger = self.trigger()
        self.assertIsNone(trigger.tick())
        self.movement("run_a")
        due = trigger.tick()
        self.advance(3599)
        self.movement("run_b", "unclassified.artifact:456")
        self.assertGreater(trigger.tick(), due)
        self.advance(3599)
        self.movement("run_passive")
        self.movement("run_review")
        trigger.tick()
        self.assertEqual(self.emissions, [])
        self.advance(1)
        trigger.tick()
        self.assertEqual(len(self.emissions), 1)
        self.assertEqual(self.emissions[0]["last_activity"]["run"], "run_b")
        self.advance(7200)
        self.trigger().tick()
        self.assertEqual(len(self.emissions), 1)
        self.movement("run_a")
        trigger = self.trigger()
        trigger.tick()
        self.advance(3600)
        trigger.tick()
        self.assertEqual(len(self.emissions), 2)

    def test_restart_recovers_admission_after_lost_ack_and_rejects_missing_evidence(self):
        self.movement("run_a")
        trigger = self.trigger()
        trigger.tick()
        self.advance(3600)
        def lost_ack(*args):
            self.admit(*args)
            raise RuntimeError("lost acknowledgement")
        with patch.object(self.module, "hand_artifact_with_evidence", lost_ack), self.assertRaisesRegex(RuntimeError, "lost acknowledgement"):
            trigger.tick()
        self.trigger().tick()
        self.assertEqual(len(self.emissions), 1)
        self.ledger.write_text("")
        with self.assertRaisesRegex(RuntimeError, "recorded arrival evidence"):
            self.trigger()

    def test_partial_tail_is_not_activity_and_truncation_is_an_error(self):
        self.movement("run_a")
        trigger = self.trigger()
        due = trigger.tick()
        with self.ledger.open("a") as out:
            out.write('{"seq":')
        self.assertEqual(trigger.tick(), due)
        self.ledger.write_text("")
        with self.assertRaisesRegex(RuntimeError, "shrank"):
            trigger.tick()

    def test_activity_during_delivery_completes_the_due_episode_once(self):
        self.movement("run_a")
        trigger = self.trigger()
        trigger.tick()
        self.advance(3600)
        def busy(*args):
            self.movement("run_b")
            return self.admit(*args)
        with patch.object(self.module, "hand_artifact_with_evidence", busy):
            trigger.tick()
        self.assertEqual([e["last_activity"]["run"] for e in self.emissions], ["run_a"])
        self.assertEqual(self.emissions[0]["activity_window"], {"after_seq": None, "through_seq": 0})
        trigger.tick()
        self.assertEqual(len(self.emissions), 1)
        self.advance(3600)
        trigger.tick()
        self.assertEqual([e["last_activity"]["run"] for e in self.emissions], ["run_a", "run_b"])
        self.assertEqual(self.emissions[1]["activity_window"]["after_seq"], 0)

    def test_scope_follows_the_ee_chain_and_a_malformed_run_only_removes_itself(self):
        ees = self.root / "ees"
        ees.mkdir()
        for ee, parent in (("ee_root", None), ("ee_project", "ee_root"), ("ee_other", "ee_root")):
            (ees / f"{ee}.json").write_text(json.dumps({"ee_id": ee, "parent_ee_id": parent}))
        slots = json.loads(os.environ["OCTOPUS_ROOT_SLOTS_JSON"])
        slots["ee_instances_root"] = str(ees)
        os.environ["OCTOPUS_ROOT_SLOTS_JSON"] = json.dumps(slots)
        for name, ee in (("run_a", "ee_project"), ("run_b", "ee_other")):
            value = json.loads((self.runs / f"{name}.json").read_text())
            (self.runs / f"{name}.json").write_text(json.dumps({**value, "ee": ee}))
        (self.runs / "run_broken.json").write_text("{")
        self.config["schedule"]["inactivity"]["within_ee"] = "ee_project"
        trigger = self.trigger()
        self.movement("run_a")
        due = trigger.tick()
        self.advance(3000)
        self.movement("run_b")
        self.assertEqual(trigger.tick(), due)

    def test_a_run_this_trigger_fed_stays_excluded_after_its_binding_is_retired(self):
        record = self.runs / 'run_review.json'
        value = json.loads(record.read_text())
        value['checkpoints'] = value['checkpoints'][:1]
        record.write_text(json.dumps(value))
        self.movement('run_review', 'cron.current_time:abc', 'InterfaceArrival', 'world_cron')
        self.movement('run_a')
        trigger = self.trigger()
        due = trigger.tick()
        self.advance(3500)
        self.movement('run_review')
        self.assertEqual(trigger.tick(), due)

    def test_runs_fed_by_excluded_interfaces_are_not_user_activity(self):
        folder = self.interfaces / 'world_other_trigger'
        folder.mkdir(parents=True)
        (folder / 'interface.json').write_text(json.dumps({'id': 'world_other_trigger', 'interface_type': 'world', 'role': 'sensor', 'interface_driver_ref': 'cron_life'}))
        for name, interfaces in (('run_bound', ['world_user', 'world_other_trigger']), ('run_retired', ['world_user'])):
            (self.runs / f'{name}.json').write_text(json.dumps({'id': name, 'checkpoints': [{'interface_binding': {'interface': item}} for item in interfaces]}))
        self.config['schedule']['inactivity']['exclude_interface_match'] = {'interface_driver_ref': 'cron_life'}
        self.movement('run_a')
        trigger = self.trigger()
        due = trigger.tick()
        self.advance(3500)
        self.movement('run_bound')
        self.movement('run_retired', 'cron.current_time:abc', 'InterfaceArrival', 'world_other_trigger')
        self.movement('run_retired')
        self.assertEqual(trigger.tick(), due)
        self.advance(100)
        trigger.tick()
        self.assertEqual(self.emissions[0]['last_activity']['run'], 'run_a')

    def test_direct_inputs_in_excluded_runs_rearm_once_without_background_loops(self):
        folder = self.interfaces / "world_other_trigger"
        folder.mkdir()
        (folder / "interface.json").write_text(json.dumps({"id": "world_other_trigger", "interface_type": "world", "role": "sensor", "interface_driver_ref": "cron_life"}))
        self.config["schedule"]["inactivity"]["exclude_interface_match"] = {"interface_driver_ref": "cron_life"}
        for run, bindings in (("run_bound", ["world_user", "world_other_trigger"]), ("run_retired", ["world_user"])):
            (self.runs / f"{run}.json").write_text(json.dumps({"id": run, "checkpoints": [{"interface_binding": {"interface": item}} for item in bindings]}))
        self.movement("run_retired", kind="InterfaceArrival", source="world_other_trigger")
        for index, run in enumerate(("run_review", "run_bound", "run_retired"), 1):
            with self.subTest(run=run):
                self.advance(7200)
                source = self.movement(run, "arbitrary.input:123", "InterfaceArrival", "world_user")
                trigger = self.trigger()
                due = trigger.tick()
                self.assertEqual(due, self.clock // 1_000_000 + 3600000)
                self.advance(3599)
                self.movement(run)
                self.movement(run, kind="InterfaceArrival", source="world_background")
                self.assertEqual(trigger.tick(), due)
                trigger = self.trigger()
                self.assertEqual(trigger.tick(), due)
                self.advance(1)
                trigger.tick()
                self.assertEqual(len(self.emissions), index)
                self.assertEqual(self.emissions[-1]["last_activity"]["seq"], source)
                self.advance(7200)
                self.trigger().tick()
                self.assertEqual(len(self.emissions), index)


if __name__ == "__main__":
    unittest.main()
