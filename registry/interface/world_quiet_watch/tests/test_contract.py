import datetime as dt
import importlib.util
import io
import json
import os
from pathlib import Path
import select
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import patch
import urllib.error

PACKAGE = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(PACKAGE))
UTC = dt.timezone.utc
ARMED = dt.datetime(2020, 1, 1, tzinfo=UTC)


class StopPoll(Exception):
    pass


class Contract(unittest.TestCase):
    def setUp(self):
        old_umask = os.umask(0o077)
        self.addCleanup(os.umask, old_umask)
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        root = Path(self.temp.name)
        self.interface, self.var, self.blobs = [root / n for n in ("interface", "var", "blobs")]
        for path in (self.interface, self.var, self.blobs):
            path.mkdir(mode=0o700)
        self.ledger = root / "movements.jsonl"
        self.ledger.write_bytes(b"")
        self.env = {
            "OCTOPUS_INTERFACE_ID": "world_test_watch",
            "OCTOPUS_INTERFACE_DIR": str(self.interface),
            "OCTOPUS_VAR_DIR": str(self.var),
            "OCTOPUS_MOVEMENTS_JSONL": str(self.ledger),
            "OCTOPUS_BLOBS_DIR": str(self.blobs),
            "OCTOPUS_INTERFACE_HOST_IO_LIMITS": json.dumps({
                "max_control_frame_bytes": 1048576, "max_manifest_bytes": 1048576,
                "max_payload_bytes": 8388608, "max_stderr_bytes": 262144}),
            "PYTHONDONTWRITEBYTECODE": "1",
        }
        spec = importlib.util.spec_from_file_location("watch_under_test", PACKAGE / "quiet_watch_life.py")
        self.w = importlib.util.module_from_spec(spec)
        with patch.dict(os.environ, self.env):
            spec.loader.exec_module(self.w)
        self.w.private_directory(self.w.PAYLOADS_DIR)
        self.configure({"terms": ["release ready", "Straße"], "match_fields": ["subject", "body"]})
        (self.interface / "armed.json").write_text(json.dumps({"armed_at": ARMED.isoformat()}))

    def configure(self, condition):
        config = {**condition, "notice_label": "test.notice", "instructions": ""}
        (self.interface / "watch.json").write_text(json.dumps(config))
        return self.w.load_watch()

    def github(self, kind="github_quiet"):
        return self.configure({kind: {"repository": "example/project",
                                     ("hours" if kind == "github_quiet" else "at_least"): 6}})

    def blob(self, value):
        body = self.w.canonical_json(value)
        digest = self.w.octopus_content_hash(body)
        (self.blobs / digest).write_bytes(body)
        return digest

    def letter(self, received=ARMED.isoformat(), subject="RELEASE   READY", body="text"):
        payload = self.blob({"message": {"subject": subject, "body": {"text": body},
                                         "received_at": received, "date": "untrusted old date"}})
        manifest = self.blob({"label": "email.event", "payload_path": "/unused",
                              "payload_content_hash": payload})
        return {"seq": 0, "kind": "InterfaceOutbound", "from": {"Interface": "world_mail"},
                "to": {"Interface": self.w.INTERFACE_ID},
                "artifact": {"content": list(bytes.fromhex(manifest))},
                "invocation_metadata": {"runtime": {
                    "run": "run_test", "ee": "ee_test", "worker": "worker_test",
                    "checkpoint": "cp_test", "interface": self.w.INTERFACE_ID, "epoch": 0}}}

    def prepared(self):
        watch = self.github("github_stars")
        path, digest = self.w.prepare_stars_notice(watch, {"stars": 6})
        state = {"schema_version": 1, "status": "prepared", "label": watch["notice_label"],
                 "manifest_path": path, "manifest_hash": digest, "source_movement_seq": 0,
                 "admitted_seq": None, "refusal": None}
        self.w.atomic_json(self.w.STATE_PATH, state)
        return state

    def arrival(self, state, seq=0):
        return {"seq": seq, "kind": "InterfaceArrival", "from": {"Interface": self.w.INTERFACE_ID},
                "artifact": {"id": self.w.artifact_id_of(state)}}

    def test_descriptors_are_generic_self_contained(self):
        descriptor = json.loads((PACKAGE / "interface.json").read_text())
        self.assertEqual((descriptor["interface_type"], descriptor["role"], descriptor["direction"]),
                         ("world", "sensor", "inbound"))
        life = json.loads((PACKAGE / "life.json").read_text())["processes"][0]
        self.assertEqual(life["read_handles"], ["ledger", "blobs"])
        for name in life["required_files"]:
            self.assertTrue((PACKAGE.parents[2] / name).is_file())
        self.assertFalse((PACKAGE / "watch.json").exists())
        self.assertFalse((PACKAGE / "armed.json").exists())

    def test_configuration_validation(self):
        valid = [
            {"terms": ["word"], "match_fields": ["body"]},
            {"github_stars": {"repository": "example/project", "at_least": 1}},
            {"github_quiet": {"repository": "example/project", "hours": 1}},
        ]
        for config in valid:
            with self.subTest(config=config):
                self.assertEqual(self.configure(config)["instructions"], "")
        invalid = [
            {"terms": [], "match_fields": ["body"]},
            {"terms": ["word"], "match_fields": ["subject", "subject"]},
            {"terms": ["word"], "match_fields": ["html"]},
            {"github_stars": {"repository": "../project", "at_least": 1}},
            {"github_stars": {"repository": "example/project", "at_least": True}},
            {"github_quiet": {"repository": "example/project", "hours": 0}},
            {"github_quiet": {"repository": "example/project", "hours": 1, "poll_seconds": 29}},
            {**valid[1], **valid[2]}, {**valid[0], "unexpected": 1},
        ]
        for config in invalid:
            with self.subTest(config=config), self.assertRaises(ValueError):
                self.configure(config)

    def test_arming_requires_offset_and_exact_shape(self):
        path = self.interface / "armed.json"
        path.unlink()
        with self.assertRaisesRegex(RuntimeError, "not armed"):
            self.w.load_armed()
        for value in [{"armed_at": "2020-01-01"}, {"armed_at": ARMED.isoformat(), "extra": 1}]:
            path.write_text(json.dumps(value))
            with self.assertRaises(ValueError):
                self.w.load_armed()
        path.write_text(json.dumps({"armed_at": ARMED.isoformat()}))
        self.assertEqual(self.w.load_armed(), ARMED)

    def test_email_eligibility_matching_and_payload(self):
        watch = self.w.load_watch()
        for received, expected in [(None, "undated"), ("2019-12-31T23:59:59+00:00", "early")]:
            with patch.object(self.w, "hand") as hand:
                self.assertEqual(self.w.process_outbound(self.letter(received), watch, ARMED), expected)
                hand.assert_not_called()
        self.assertIsNone(self.w.process_outbound(self.letter(subject="nothing"), watch, ARMED))
        with patch.object(self.w, "hand", return_value=3):
            state = self.w.process_outbound(self.letter(body="STRASSE"), watch, ARMED)
        self.assertEqual(state["status"], "admitted")
        manifest = json.loads(Path(state["manifest_path"]).read_text())
        payload = json.loads(Path(manifest["payload_path"]).read_text())
        self.assertEqual(payload["trigger"]["matched_words"], ["release ready", "Straße"])
        self.assertEqual(payload["from_owner"], "")
        self.assertEqual(payload["message"]["date"], "untrusted old date")

    def test_blob_hash_and_outbound_provenance_fail_closed(self):
        movement = self.letter()
        self.assertTrue(self.w.is_own_outbound(movement))
        movement["invocation_metadata"]["runtime"]["interface"] = "other"
        with self.assertRaises(ValueError):
            self.w.is_own_outbound(movement)
        digest = bytes(movement["artifact"]["content"]).hex()
        (self.blobs / digest).write_text("{}")
        with self.assertRaisesRegex(ValueError, "content hash"):
            self.w.read_artifact(movement)

    def test_prepared_restart_verifies_bytes_and_recovers_admission(self):
        state = self.prepared()
        self.w.verify_prepared(self.w.load_state())
        self.ledger.write_text(json.dumps(self.arrival(state)) + "\n")
        with patch.object(self.w, "hand") as hand:
            result = self.w.deliver(self.w.load_state())
            hand.assert_not_called()
        self.assertEqual((result["status"], result["admitted_seq"]), ("admitted", 0))
        self.assertEqual(self.w.load_state(), result)
        Path(state["manifest_path"]).write_text("{}")
        with self.assertRaisesRegex(ValueError, "recorded hash"):
            self.w.verify_prepared(state)

    def test_refused_and_transient_transitions(self):
        for reason, detail, transient in [
            ("registry_unstable", {}, True),
            ("stale_epoch", {"reason_detail": "arrival_target_changed"}, True),
            ("stale_epoch", {"reason_detail": "other"}, False),
            ("policy.interface_binding_denied", {}, False),
        ]:
            with self.subTest(reason=reason, detail=detail):
                state = self.prepared()
                refused = self.w.Refused({"refused": {"reason": reason, "detail": detail}})
                with patch.object(self.w, "hand", side_effect=[refused, 9]) as hand, patch.object(self.w.time, "sleep"):
                    result = self.w.deliver(state)
                self.assertEqual(result["status"], "admitted" if transient else "refused")
                self.assertEqual(hand.call_count, 2 if transient else 1)
                self.assertEqual(self.w.load_state(), result)

    def test_duplicate_refusal_requires_exact_ledger_evidence(self):
        state = self.prepared()
        refusal = self.w.Refused({"refused": {"reason": "stale_epoch", "detail": {
            "artifact_id": self.w.artifact_id_of(state), "message": self.w.DUPLICATE_MESSAGE}}})
        with patch.object(self.w, "hand", side_effect=refusal), patch.object(
                self.w, "find_admitted_arrival_seq", side_effect=[None, 7]):
            self.assertEqual(self.w.deliver(state)["admitted_seq"], 7)
        with patch.object(self.w, "hand", side_effect=refusal), patch.object(
                self.w, "find_admitted_arrival_seq", return_value=None), self.assertRaisesRegex(
                    RuntimeError, "ledger has no such arrival"):
            self.w.deliver(state)

    def test_lost_verdict_preserves_prepared_identity(self):
        state = self.prepared()
        with patch.object(self.w, "hand", side_effect=RuntimeError("closed")), self.assertRaises(RuntimeError):
            self.w.deliver(state)
        self.assertEqual(self.w.load_state(), state)
        self.w.verify_prepared(state)

    def test_real_hand_protocol_and_mismatched_verdict(self):
        for response, fails in [
            ({"kind": "delivery_verdict", "request_id": 1, "verdict": {"admitted": {"seq": 4}}}, False),
            ({"kind": "delivery_verdict", "request_id": 2, "verdict": {"admitted": {"seq": 4}}}, True),
        ]:
            self.w._NEXT_REQUEST_ID = 1
            stdin = type("Input", (), {"buffer": io.BytesIO(json.dumps(response).encode() + b"\n")})()
            stdout = type("Output", (), {"buffer": io.BytesIO()})()
            with patch.object(self.w.sys, "stdin", stdin), patch.object(self.w.sys, "stdout", stdout):
                if fails:
                    with self.assertRaises(RuntimeError):
                        self.w.hand("/manifest", [])
                else:
                    self.assertEqual(self.w.hand("/manifest", []), 4)
            frame = json.loads(stdout.buffer.getvalue())
            self.assertEqual((frame["kind"], frame["request_id"]), ("arrival", 1))

    def test_github_api_uses_committer_date_and_etags(self):
        response = type("Response", (), {
            "headers": {"ETag": '"v1"'}, "__enter__": lambda s: s, "__exit__": lambda *a: None,
            "read": lambda s, n: json.dumps([{"commit": {
                "committer": {"date": "2020-01-01T00:00:00Z"},
                "author": {"date": "2040-01-01T00:00:00Z"}},
                "pushed_at": "2040-01-01T00:00:00Z"}]).encode()})()
        with patch.object(self.w.urllib.request, "urlopen", return_value=response) as request:
            observed, etag, problem, _ = self.w.read_last_commit("example/project", '"old"')
        self.assertEqual(observed, ARMED)
        self.assertIsNone(problem)
        self.assertEqual(etag, '"v1"')
        self.assertTrue(request.call_args.args[0].full_url.endswith("/commits?per_page=1"))
        self.assertEqual(request.call_args.args[0].get_header("If-none-match"), '"old"')
        for code, headers in [(304, {}), (429, {"X-RateLimit-Reset": "100"})]:
            with patch.object(self.w.urllib.request, "urlopen", side_effect=urllib.error.HTTPError(
                    "https://api.github.com", code, "test", headers, None)), patch.object(self.w.time, "time", return_value=90):
                result = self.w.read_stars("example/project", etag)
            self.assertEqual(result[:2], (None, etag))
            self.assertEqual(result[3], None if code == 304 else 15)

    def test_github_thresholds_future_arming_and_spent_state(self):
        for kind in ("github_stars", "github_quiet"):
            watch = self.github(kind)
            method = self.w.watch_stars if kind == "github_stars" else self.w.watch_quiet
            reader = "read_stars" if kind == "github_stars" else "read_last_commit"
            observation = 6 if kind == "github_stars" else ARMED
            with patch.object(self.w, reader, return_value=(observation, None, None, None)), patch.object(
                    self.w, "deliver", side_effect=lambda s: s):
                self.assertEqual(method(watch, ARMED, None)["status"], "prepared")
            future = dt.datetime.now(UTC) + dt.timedelta(days=1)
            with patch.object(self.w, "load_armed", return_value=future), patch.object(
                    self.w, reader) as read, patch.object(self.w.time, "sleep", side_effect=StopPoll):
                with self.assertRaises(StopPoll):
                    method(watch, future, None)
                read.assert_not_called()
            for status in ("refused", "admitted"):
                with patch.object(self.w, reader) as read:
                    self.assertEqual(method(watch, ARMED, {"status": status}), {"status": status})
                    read.assert_not_called()

    def test_cursor_partial_rows_restart_and_spent_email(self):
        movement = self.letter()
        self.ledger.write_text(json.dumps(movement) + "\n" + '{"seq":1')
        with patch.object(self.w, "hand", return_value=2) as hand:
            cursor, state = self.w.follow(self.w.empty_cursor(), self.w.load_watch(), ARMED, None)
            self.assertEqual(cursor["next_seq"], 1)
            self.assertEqual(self.w.load_cursor(), cursor)
            self.w.follow(self.w.load_cursor(), self.w.load_watch(), ARMED, state)
            self.assertEqual(hand.call_count, 1)
        with self.ledger.open("a") as ledger:
            ledger.write(',"kind":"other"}\n')
        cursor, _ = self.w.follow(cursor, self.w.load_watch(), ARMED, state)
        self.assertEqual(cursor["next_seq"], 2)
        self.ledger.write_text('{"seq":3}\n')
        with self.assertRaisesRegex(RuntimeError, "discontinuity"):
            list(self.w.movements_after(cursor))


    def test_poll_threshold_boundary_304_and_error_do_not_fabricate_delivery(self):
        now = ARMED + dt.timedelta(hours=6)
        class Clock(dt.datetime):
            @classmethod
            def now(cls, tz=None):
                return now
        for first_date, second_result, fires in [
            (ARMED, None, True),
            (ARMED + dt.timedelta(seconds=1), (None, '"v1"', None, None), False),
            (ARMED + dt.timedelta(seconds=1), (None, '"v1"', "API unavailable", None), False),
        ]:
            watch = self.github()
            polls = [(first_date, '"v1"', None, None)]
            if second_result:
                polls.append(second_result)
            sleeps = 0
            def sleep(_):
                nonlocal now, sleeps
                sleeps += 1
                now += dt.timedelta(seconds=2)
                if sleeps >= len(polls):
                    raise StopPoll()
            now = ARMED + dt.timedelta(hours=6)
            with patch.object(self.w.datetime, "datetime", Clock), patch.object(
                    self.w, "read_last_commit", side_effect=polls), patch.object(
                    self.w.time, "sleep", side_effect=sleep), patch.object(
                    self.w, "deliver", side_effect=lambda state: state) as hand:
                # A 304 may complete a valid cached observation as its age crosses the boundary.
                should_fire = fires or (second_result is not None and second_result[2] is None)
                if should_fire:
                    self.assertEqual(self.w.watch_quiet(watch, ARMED, None)["status"], "prepared")
                else:
                    with self.assertRaises(StopPoll):
                        self.w.watch_quiet(watch, ARMED, None)
                self.assertEqual(hand.called, should_fire)

    def test_missing_committer_date_has_no_author_or_push_fallback(self):
        for commit in ({}, {"committer": {"date": "not-a-time"}},
                       {"committer": {"date": "2020-01-01T00:00:00"}}):
            response = type("Response", (), {
                "headers": {}, "__enter__": lambda s: s, "__exit__": lambda *a: None,
                "read": lambda s, n: json.dumps([{
                    "commit": {"author": {"date": "2020-01-01T00:00:00Z"}, **commit},
                    "pushed_at": "2020-01-01T00:00:00Z"}]).encode()})()
            with patch.object(self.w.urllib.request, "urlopen", return_value=response):
                with self.assertRaises((KeyError, ValueError)):
                    self.w.read_last_commit("example/project", None)

    def test_life_process_admission_and_restart_stay_one_shot(self):
        self.ledger.write_text(json.dumps(self.letter()) + "\n")
        def start():
            proc = subprocess.Popen([sys.executable, str(PACKAGE / "quiet_watch_life.py")],
                                    env={**os.environ, **self.env}, stdin=subprocess.PIPE,
                                    stdout=subprocess.PIPE, stderr=subprocess.PIPE, bufsize=0)
            def stop():
                if proc.poll() is None:
                    proc.terminate()
                proc.wait(timeout=5)
                for stream in (proc.stdin, proc.stdout, proc.stderr):
                    stream.close()
            self.addCleanup(stop)
            return proc, stop
        def frame(proc):
            self.assertTrue(select.select([proc.stdout], [], [], 5)[0], "life produced no frame")
            return json.loads(proc.stdout.readline())
        proc, stop = start()
        self.assertEqual(frame(proc)["kind"], "ready")
        notice = frame(proc)
        self.assertEqual(notice["kind"], "arrival")
        state = self.w.load_state()
        self.assertEqual(state["status"], "prepared")
        with self.ledger.open("a") as ledger:
            ledger.write(json.dumps(self.arrival(state, 1)) + "\n")
        proc.stdin.write(json.dumps({"kind": "delivery_verdict", "request_id": notice["request_id"],
                                     "verdict": {"admitted": {"seq": 1}}}).encode() + b"\n")
        proc.stdin.flush()
        deadline = time.monotonic() + 5
        while self.w.load_state()["status"] != "admitted" and time.monotonic() < deadline:
            time.sleep(.01)
        self.assertEqual(self.w.load_state()["status"], "admitted")
        stop()
        proc, _ = start()
        self.assertEqual(frame(proc)["kind"], "ready")
        self.assertFalse(select.select([proc.stdout], [], [], .4)[0], "restart emitted another notice")
        self.assertIsNone(proc.poll())



    def test_email_fields_do_not_cross_and_missing_received_time_fails(self):
        watch = self.w.load_watch()
        self.assertEqual(self.w.matched_terms(watch, {"subject": "release", "body": "ready"}), [])
        movement = self.letter()
        _, digest = self.w.read_artifact(movement)
        event = json.loads((self.blobs / digest).read_text())
        del event["message"]["received_at"]
        with self.assertRaisesRegex(ValueError, "no received_at"):
            self.w.read_letter(0, self.blob(event))

    def test_stars_api_value_and_below_threshold(self):
        response = type("Response", (), {
            "headers": {"ETag": '"stars"'}, "__enter__": lambda s: s, "__exit__": lambda *a: None,
            "read": lambda s, n: b'{"stargazers_count":5}'})()
        with patch.object(self.w.urllib.request, "urlopen", return_value=response):
            result = self.w.read_stars("example/project", None)
        self.assertEqual(result, (5, '"stars"', None, None))
        with patch.object(self.w, "read_stars", return_value=result), patch.object(
                self.w.time, "sleep", side_effect=StopPoll), patch.object(self.w, "deliver") as hand:
            with self.assertRaises(StopPoll):
                self.w.watch_stars(self.github("github_stars"), ARMED, None)
            hand.assert_not_called()

    def test_private_state_rejects_symlink_and_corrupt_admission(self):
        state = self.prepared()
        Path(self.w.STATE_PATH).unlink()
        Path(self.w.STATE_PATH).symlink_to(self.interface / "watch.json")
        with self.assertRaises(PermissionError):
            self.w.load_state()
        Path(self.w.STATE_PATH).unlink()
        self.w.atomic_json(self.w.STATE_PATH, {**state, "status": "admitted", "admitted_seq": 0})
        with patch.object(self.w, "signal_ready") as ready, self.assertRaisesRegex(
                RuntimeError, "ledger holds no such arrival"):
            self.w.main()
        ready.assert_not_called()


if __name__ == "__main__":
    unittest.main()
