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

    def place_runs(self, placement):
        ees = self.root / "ees"
        ees.mkdir()
        for ee, parent in (("ee_root", None), ("ee_project", "ee_root"), ("ee_other", None)):
            (ees / f"{ee}.json").write_text(json.dumps({"ee_id": ee, "parent_ee_id": parent}))
        slots = json.loads(os.environ["OCTOPUS_ROOT_SLOTS_JSON"])
        slots["ee_instances_root"] = str(ees)
        os.environ["OCTOPUS_ROOT_SLOTS_JSON"] = json.dumps(slots)
        for name, ee in placement.items():
            (self.runs / f"{name}.json").write_text(json.dumps({"id": name, "ee": ee, "checkpoints": [{"interface_binding": {"interface": "world_user"}}]}))

    def test_within_ee_counts_child_ee_runs_and_exact_ee_does_not(self):
        self.place_runs({"run_a": "ee_root", "run_b": "ee_project", "run_c": "ee_other"})
        self.config["schedule"]["inactivity"]["within_ee"] = "ee_root"
        trigger = self.trigger()
        self.movement("run_a")
        due = trigger.tick()
        self.advance(600)
        self.movement("run_c")
        self.assertEqual(trigger.tick(), due)
        self.movement("run_b")
        self.assertEqual(trigger.tick(), due + 600_000)

        del self.config["schedule"]["inactivity"]["within_ee"]
        self.config["schedule"]["inactivity"]["exact_ee"] = "ee_root"
        trigger = self.trigger()
        self.assertEqual(trigger.tick(), due)
        self.advance(600)
        self.movement("run_b")
        self.movement("run_c")
        self.assertEqual(trigger.tick(), due)
        self.movement("run_a")
        self.assertEqual(trigger.tick(), due + 1_200_000)

    def test_run_ids_ignore_sibling_and_child_runs_even_within_their_ee(self):
        self.place_runs({"run_a": "ee_root", "run_b": "ee_project", "run_c": "ee_root"})
        self.config["schedule"]["inactivity"].update(within_ee="ee_root", run_ids=["run_a"])
        trigger = self.trigger()
        self.movement("run_b")
        self.movement("run_c")
        self.assertIsNone(trigger.tick())
        self.movement("run_a")
        due = trigger.tick()
        self.advance(3599)
        self.movement("run_b")
        self.movement("run_c")
        self.assertEqual(trigger.tick(), due)
        self.assertEqual(self.emissions, [])
        self.advance(1)
        trigger.tick()
        self.assertEqual([e["last_activity"]["run"] for e in self.emissions], ["run_a"])
        self.advance(7200)
        self.movement("run_b")
        self.movement("run_c")
        trigger.tick()
        self.advance(7200)
        trigger.tick()
        self.assertEqual(len(self.emissions), 1)
        self.movement("run_a")
        self.assertEqual(trigger.tick(), self.clock // 1_000_000 + 3_600_000)

    def test_a_listed_run_outside_the_other_scopes_is_not_watched(self):
        self.place_runs({"run_a": "ee_other", "run_b": "ee_root"})
        self.config["schedule"]["inactivity"].update(exact_ee="ee_root", run_ids=["run_a"])
        trigger = self.trigger()
        self.movement("run_a")
        self.movement("run_b")
        self.assertIsNone(trigger.tick())

    def test_results_from_peers_do_not_keep_a_watched_run_active(self):
        folder = self.interfaces / "system_peer_input"
        folder.mkdir()
        (folder / "interface.json").write_text(json.dumps({"id": "system_peer_input", "interface_type": "system", "role": "service"}))
        (self.runs / "run_a.json").write_text(json.dumps({"id": "run_a", "checkpoints": [{"interface_binding": {"interface": item}} for item in ("world_user", "system_peer_input")]}))
        self.config["schedule"]["inactivity"].update(run_ids=["run_a"], exclude_interface_match={"interface_type": "system", "role": "service"})
        trigger = self.trigger()
        source = self.movement("run_a", kind="InterfaceArrival", source="world_user")
        due = trigger.tick()
        self.advance(1800)
        self.movement("run_a", "peer.result:1", "InterfaceArrival", "system_peer_input")
        self.movement("run_a")
        self.movement("run_b", kind="InterfaceArrival", source="world_user")
        self.assertEqual(trigger.tick(), due)
        self.advance(1800)
        self.movement("run_a", "peer.result:2", "InterfaceArrival", "system_peer_input")
        trigger.tick()
        self.assertEqual(len(self.emissions), 1)
        self.assertEqual(self.emissions[0]["last_activity"], {"run": "run_a", "seq": source, "recorded_at_unix_ms": due - 3_600_000})
        self.advance(60)
        self.movement("run_a", kind="InterfaceArrival", source="world_user")
        self.assertEqual(trigger.tick(), self.clock // 1_000_000 + 3_600_000)

    def test_arrival_artifact_filter_ignores_context_peer_and_outbound_noise(self):
        self.config["schedule"]["inactivity"].update(
            run_ids=["run_a"], direction="arrival",
            artifact_names=["turn.agent_message", "turn.completed"])
        trigger = self.trigger()
        self.movement("run_a", "interface.context.updated:1", "InterfaceArrival")
        self.assertIsNone(trigger.tick())
        self.movement("run_a", "turn.agent_message:1", "InterfaceArrival")
        due = trigger.tick()
        self.advance(3599)
        self.movement("run_a", "interface.context.updated:2", "InterfaceArrival")
        self.movement("run_a", "turn.agent_message.extra:1", "InterfaceArrival")
        self.movement("run_a", "turn.completed:", "InterfaceArrival")
        self.movement("run_a", "turn.completed:1", "InterfaceArrival", "world_background")
        self.movement("run_a", "turn.completed:1")
        self.assertEqual(trigger.tick(), due)
        self.advance(1)
        trigger.tick()
        self.assertEqual(len(self.emissions), 1)
        self.advance(3600)
        self.trigger().tick()
        self.assertEqual(len(self.emissions), 1)
        self.movement("run_a", "turn.completed:2", "InterfaceArrival")
        self.assertEqual(trigger.tick(), self.clock // 1_000_000 + 3_600_000)

    def test_unmatched_trigger_arrival_still_excludes_autonomous_activity(self):
        record = self.runs / "run_review.json"
        value = json.loads(record.read_text())
        value["checkpoints"] = value["checkpoints"][:1]
        record.write_text(json.dumps(value))
        self.config["schedule"]["inactivity"]["artifact_names"] = ["turn.completed"]
        self.movement("run_review", "cron.current_time:abc", "InterfaceArrival", "world_cron")
        self.movement("run_review", "turn.completed:123")
        self.assertIsNone(self.trigger().tick())
        self.movement("run_a", "turn.completed:234")
        self.assertIsNotNone(self.trigger().tick())

    def test_outbound_activity_excludes_matching_arrivals_and_verification_rows(self):
        self.config["schedule"]["inactivity"].update(
            direction="outbound", artifact_names=["turn.completed"])
        trigger = self.trigger()
        self.movement("run_a", "turn.completed:1", "InterfaceArrival")
        self.movement("run_a", "turn.completed:2", "VerificationFailed")
        self.assertIsNone(trigger.tick())
        seq = self.movement("run_a", "turn.completed:3")
        trigger.tick()
        self.advance(3600)
        self.movement("run_a", "turn.completed:4", "InterfaceArrival")
        trigger.tick()
        self.assertEqual([e["last_activity"]["seq"] for e in self.emissions], [seq])

    def test_filtered_lost_ack_remains_consumed_after_filter_edit(self):
        condition = self.config["schedule"]["inactivity"]
        condition.update(direction="arrival", artifact_names=["turn.agent_message"])
        self.movement("run_a", "turn.completed:older", "InterfaceArrival")
        first = self.movement("run_a", "turn.agent_message:1", "InterfaceArrival")
        trigger = self.trigger()
        trigger.tick()
        self.advance(3600)

        def lost_ack(*args):
            self.admit(*args)
            raise RuntimeError("lost acknowledgement")

        with patch.object(self.module, "hand_artifact_with_evidence", lost_ack), self.assertRaisesRegex(RuntimeError, "lost acknowledgement"):
            trigger.tick()
        condition["artifact_names"] = ["turn.completed"]
        recovered = self.trigger()
        self.assertIsNone(recovered.tick())
        self.assertEqual(recovered.state["consumed_seq"], first)
        second = self.movement("run_a", "turn.completed:new", "InterfaceArrival")
        recovered.tick()
        self.advance(3600)
        recovered.tick()
        self.assertEqual([e["last_activity"]["seq"] for e in self.emissions], [first, second])

    def test_movement_filters_are_validated(self):
        for field, value in (("artifact_names", []), ("artifact_names", "turn.completed"),
                             ("artifact_names", ["", 1]), ("artifact_names", ["x", "x"]),
                             ("artifact_names", ["two words"]), ("artifact_names", ["🐙" * 65]),
                             ("artifact_names", ["\ud800"]), ("direction", "incoming"),
                             ("direction", None), ("direction", ["arrival"])):
            with self.subTest(field=field, value=value):
                self.config["schedule"]["inactivity"][field] = value
                with self.assertRaisesRegex(ValueError, field):
                    self.schedule()
                del self.config["schedule"]["inactivity"][field]

    def test_scope_fields_are_validated(self):
        for field, value in (("run_ids", []), ("run_ids", "run_a"), ("run_ids", ["run_a", "run_a"]), ("run_ids", ["Run A"]), ("run_ids", [7]), ("exact_ee", ""), ("exact_ee", "EE Root"), ("exact_ee", ["ee_root"])):
            with self.subTest(field=field, value=value):
                self.config["schedule"]["inactivity"][field] = value
                with self.assertRaisesRegex(ValueError, field):
                    self.schedule()
                del self.config["schedule"]["inactivity"][field]
        self.config["schedule"]["inactivity"].update(run_ids=["run_a", "run_b"], exact_ee="ee_root", within_ee="ee_root")
        self.assertEqual(self.schedule()["inactivity"]["run_ids"], ["run_a", "run_b"])

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
