import json
import os
import queue
import shutil
import subprocess
import sys
import tempfile
import threading
import time
import unittest

sys.dont_write_bytecode = True

KIT = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(os.path.dirname(KIT))
SENSOR_DIR = os.path.join(KIT, "registry", "interface", "world_legacy_watch")
LIFE = os.path.join(SENSOR_DIR, "legacy_watch_life.py")
AUTHOR = os.path.join(KIT, "author.py")
sys.path.insert(0, SENSOR_DIR)

from octopus_content_hash import octopus_content_hash

with open(os.path.join(SENSOR_DIR, "life.json")) as handle:
    IO_LIMITS = json.load(handle)["processes"][0]["io_limits"]

SENDER = "Alice Example"
SENDER_ADDRESS = "alice@example.org"
RECIPIENT_ADDRESS = "bob@example.net"
COPY_ADDRESS = "carol@example.com"
MESSAGE_ID = "<m-legacy-1@example.org>"
PRIVATE_STRINGS = (SENDER, SENDER_ADDRESS, RECIPIENT_ADDRESS, COPY_ADDRESS, MESSAGE_ID, "uidvalidity", "received_at")
ARMED_AT = "2026-11-03T10:00:00+00:00"
AFTER_ARMING = "2026-11-03T10:00:01+00:00"
BEFORE_ARMING = "2026-11-03T12:59:59+03:00"


def canonical(value):
    return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode("utf-8")


class FakeRuntime:
    def __init__(self, root):
        self.root = root
        self.var = os.path.join(root, "var")
        self.interface = os.path.join(root, "interface")
        shutil.copytree(SENSOR_DIR, self.interface, ignore=shutil.ignore_patterns("__pycache__"))
        with open(os.path.join(self.interface, "armed.json"), "w") as handle:
            json.dump({"armed_at": ARMED_AT}, handle)
        self.ledger = os.path.join(root, "ledger", "movements.jsonl")
        self.blobs = os.path.join(root, "ledger", "blobs")
        os.makedirs(self.blobs)
        self.seq = 0
        self.uid = 0
        self.process = None
        self.frames = None
        self.stderr = []

    def append(self, row):
        row = {"seq": self.seq, "lamport": self.seq + 1, "recorded_at_unix_ms": 1_700_000_000_000 + self.seq, "trace": f"trace:{self.seq}", **row}
        with open(self.ledger, "ab") as handle:
            handle.write(json.dumps(row).encode("utf-8") + b"\n")
        self.seq += 1
        return row["seq"]

    def append_raw(self, line):
        with open(self.ledger, "ab") as handle:
            handle.write(line)

    def put_blob(self, content):
        digest = octopus_content_hash(content)
        with open(os.path.join(self.blobs, digest), "wb") as handle:
            handle.write(content)
        return digest

    def noise(self):
        return self.append({"kind": "InterfaceArrival", "from": {"Interface": "world_cron"}, "to": {"Interface": "system_worker_cron"}, "artifact": {"id": "cron.tick:x", "content": "0" * 64}, "invocation_metadata": {}})

    def letter(self, subject, body, as_list=False, received_at=AFTER_ARMING):
        self.uid += 1
        event = {
            "schema_version": 1,
            "event": "message.received",
            "account": RECIPIENT_ADDRESS,
            "trusted": False,
            "sender": {"address": SENDER_ADDRESS, "name": SENDER, "allowed_as": "any_sender", "verified": False, "verification": "none"},
            "message": {
                "ref": {"mailbox": "INBOX", "uidvalidity": 7, "uid": self.uid},
                "message_id": MESSAGE_ID,
                "in_reply_to": [],
                "references": [],
                "date": f"2026-11-0{self.uid % 9 + 1}T10:00:00+00:00",
                "received_at": received_at,
                "from": {"address": SENDER_ADDRESS, "name": SENDER},
                "reply_to": [],
                "to": [{"address": RECIPIENT_ADDRESS, "name": ""}],
                "cc": [{"address": COPY_ADDRESS, "name": ""}],
                "subject": subject,
                "body": {"text": body, "source": "text/plain", "truncated": False},
                "attachments": [],
                "omitted_attachments": 0,
                "inline_parts": 0,
                "flags": [],
            },
        }
        payload = canonical(event)
        payload_hash = self.put_blob(payload)
        manifest = canonical({"label": "email.event", "payload_path": f"/hand-in/world_email/{payload_hash}.payload", "payload_content_hash": payload_hash})
        manifest_hash = self.put_blob(manifest)
        artifact = {"id": f"email.event:{manifest_hash}", "content": list(bytes.fromhex(manifest_hash)) if as_list else manifest_hash}
        self.append({"kind": "InterfaceArrival", "from": {"Interface": "world_email"}, "to": {"Interface": "system_worker_legacy_relay"}, "artifact": artifact, "invocation_metadata": {}})
        return self.append({
            "kind": "InterfaceOutbound",
            "from": {"Interface": "world_email"},
            "to": {"Interface": "world_legacy_watch"},
            "artifact": artifact,
            "invocation_metadata": {"runtime": {"run": "run_legacy_watch_relay", "ee": "ee_existence", "worker": "system_worker_legacy_relay", "checkpoint": "cp_legacy_watch_email", "interface": "world_legacy_watch", "epoch": 0}},
        })

    def start(self, ready=True):
        environment = {
            "PATH": os.environ.get("PATH", ""),
            "PYTHONDONTWRITEBYTECODE": "1",
            "OCTOPUS_INTERFACE_ID": "world_legacy_watch",
            "OCTOPUS_INTERFACE_DIR": self.interface,
            "OCTOPUS_VAR_DIR": self.var,
            "OCTOPUS_MOVEMENTS_JSONL": self.ledger,
            "OCTOPUS_BLOBS_DIR": self.blobs,
            "OCTOPUS_INTERFACE_HOST_IO_LIMITS": json.dumps(IO_LIMITS),
        }
        self.process = subprocess.Popen([sys.executable, LIFE], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=environment)
        self.frames = queue.Queue()
        self.stderr = []
        process, frames, stderr = self.process, self.frames, self.stderr
        threading.Thread(target=lambda: [frames.put(json.loads(line)) for line in process.stdout], daemon=True).start()
        threading.Thread(target=lambda: [stderr.append(line.decode("utf-8", "replace")) for line in process.stderr], daemon=True).start()
        if ready:
            self.expect_frame("ready")

    def stop(self):
        if self.process is not None and self.process.poll() is None:
            self.process.kill()
        if self.process is not None:
            self.process.wait(timeout=10)
            self.process.stdin.close()
            self.process.stdout.close()
            self.process.stderr.close()
        self.process = None

    def expect_frame(self, kind, timeout=10):
        try:
            frame = self.frames.get(timeout=timeout)
        except queue.Empty:
            raise AssertionError(f"no {kind} frame; stderr: {''.join(self.stderr)}") from None
        if frame.get("kind") != kind:
            raise AssertionError(f"expected {kind}, got {frame}")
        return frame

    def expect_no_frame(self, duration=0.8):
        try:
            frame = self.frames.get(timeout=duration)
        except queue.Empty:
            return
        raise AssertionError(f"unexpected frame {frame}")

    def wait_cursor(self, timeout=10):
        deadline = time.monotonic() + timeout
        path = os.path.join(self.var, "ledger-cursor.json")
        while time.monotonic() < deadline:
            if self.process.poll() is not None:
                raise AssertionError(f"life exited {self.process.returncode}: {''.join(self.stderr)}")
            try:
                with open(path) as handle:
                    if json.load(handle)["next_seq"] >= self.seq:
                        return
            except (FileNotFoundError, json.JSONDecodeError):
                pass
            time.sleep(0.05)
        raise AssertionError(f"cursor did not reach {self.seq}; stderr: {''.join(self.stderr)}")

    def admit(self, frame):
        with open(frame["artifact_manifest_path"], "rb") as handle:
            manifest_bytes = handle.read()
        manifest = json.loads(manifest_bytes)
        with open(manifest["payload_path"], "rb") as handle:
            payload = handle.read()
        artifact_id = f"{manifest['label']}:{octopus_content_hash(manifest_bytes)}"
        seq = self.append({"kind": "InterfaceArrival", "from": {"Interface": "world_legacy_watch"}, "to": {"Interface": "system_worker_operator_task_x"}, "artifact": {"id": artifact_id, "content": octopus_content_hash(manifest_bytes)}, "invocation_metadata": {}})
        self.reply(frame, {"admitted": {"seq": seq}})
        return manifest_bytes, manifest, payload, artifact_id, seq

    def reply(self, frame, verdict):
        self.process.stdin.write(canonical({"kind": "delivery_verdict", "request_id": frame["request_id"], "verdict": verdict}) + b"\n")
        self.process.stdin.flush()

    def refuse(self, frame, reason, detail):
        self.reply(frame, {"refused": {"reason": reason, "detail": detail}})

    def fired(self):
        try:
            with open(os.path.join(self.var, "fired.json")) as handle:
                return json.load(handle)
        except FileNotFoundError:
            return None


class LegacyWatchLifeTest(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.mkdtemp(prefix="legacy-watch-life-")
        self.runtime = FakeRuntime(self.directory)

    def tearDown(self):
        self.runtime.stop()
        shutil.rmtree(self.directory)

    def test_only_letters_received_after_arming_fire_even_when_relayed_before_the_first_start(self):
        runtime = self.runtime
        runtime.noise()
        runtime.letter("Your Digital Legacy settings", "A Legacy Contact was added.", received_at=BEFORE_ARMING)
        runtime.letter("Death certificate", "Undated letter.", received_at=None)
        runtime.letter("Re: legacy contact", "Arrived after arming, relayed before the sensor started.")
        runtime.start()
        frame = runtime.expect_frame("arrival")
        _, _, payload, _, _ = runtime.admit(frame)
        self.assertEqual(json.loads(payload)["message"]["subject"], "Re: legacy contact")
        runtime.wait_cursor()
        runtime.expect_no_frame()

    def test_a_letter_without_any_term_emits_nothing(self):
        runtime = self.runtime
        runtime.noise()
        runtime.start()
        runtime.letter("Lunch on Friday?", "Shall we meet at noon near the old library?")
        runtime.wait_cursor()
        runtime.expect_no_frame()
        self.assertIsNone(runtime.fired())

    def test_the_first_matching_letter_fires_once_with_only_subject_date_and_body(self):
        runtime = self.runtime
        runtime.noise()
        runtime.start()
        runtime.letter("Weekly digest", "Nothing to see here.")
        body = "Hello,\nplease review the dIgItAl\n   LEGACY request we received.\nRegards"
        runtime.letter("Quarterly update", body, as_list=True)
        frame = runtime.expect_frame("arrival")
        manifest_bytes, manifest, payload, artifact_id, seq = runtime.admit(frame)
        self.assertEqual(set(manifest), {"label", "payload_path", "payload_content_hash"})
        self.assertEqual(manifest["label"], "legacy_watch.notice")
        self.assertEqual(manifest["payload_content_hash"], octopus_content_hash(payload))
        notice = json.loads(payload)
        self.assertEqual(set(notice), {"trigger", "from_owner", "message"})
        self.assertEqual(set(notice["trigger"]), {"event", "watched_fields", "watched_words", "matched_words"})
        self.assertEqual(notice["trigger"]["matched_words"], ["Digital Legacy"])
        self.assertEqual(notice["trigger"]["watched_fields"], ["subject", "body"])
        self.assertIn("Legacy Contact", notice["trigger"]["watched_words"])
        with open(os.path.join(SENSOR_DIR, "watch.json"), encoding="utf-8") as handle:
            self.assertEqual(notice["from_owner"], json.load(handle)["instructions"])
        self.assertEqual(notice["message"], {"subject": "Quarterly update", "date": "2026-11-03T10:00:00+00:00", "body": body})
        evidence = frame["evidence"]
        self.assertEqual(len(evidence), 1)
        self.assertEqual(evidence[0]["artifact_id"], artifact_id)
        self.assertEqual(evidence[0]["bytes"], list(bytes.fromhex(octopus_content_hash(artifact_id.encode("utf-8")))))
        for private in PRIVATE_STRINGS:
            self.assertNotIn(private.encode("utf-8"), manifest_bytes + payload + canonical(frame))
        runtime.letter("СВИДЕТЕЛЬСТВО О СМЕРТИ", "Second matching letter.")
        runtime.letter("deceased", "Third matching letter.")
        runtime.wait_cursor()
        runtime.expect_no_frame()
        fired = runtime.fired()
        self.assertEqual((fired["status"], fired["admitted_seq"], fired["manifest_path"]), ("admitted", seq, frame["artifact_manifest_path"]))

    def test_a_restart_while_prepared_hands_the_identical_manifest_again(self):
        runtime = self.runtime
        runtime.start()
        runtime.letter("Death certificate", "Please find the scan attached.")
        first = runtime.expect_frame("arrival")
        with open(first["artifact_manifest_path"], "rb") as handle:
            first_manifest = handle.read()
        runtime.stop()
        self.assertEqual(runtime.fired()["status"], "prepared")
        runtime.letter("Цифровое наследство", "A different matching letter arrived while the life was down.")
        runtime.start()
        second = runtime.expect_frame("arrival")
        self.assertEqual(second, first)
        with open(second["artifact_manifest_path"], "rb") as handle:
            self.assertEqual(handle.read(), first_manifest)
        _, _, payload, _, seq = runtime.admit(second)
        self.assertEqual(json.loads(payload)["message"]["subject"], "Death certificate")
        runtime.wait_cursor()
        runtime.expect_no_frame()
        self.assertEqual((runtime.fired()["status"], runtime.fired()["admitted_seq"]), ("admitted", seq))

    def test_an_admission_recorded_before_a_crash_is_not_handed_again(self):
        runtime = self.runtime
        runtime.start()
        runtime.letter("Re: legacy contact", "Confirming.")
        frame = runtime.expect_frame("arrival")
        with open(frame["artifact_manifest_path"], "rb") as handle:
            manifest_bytes = handle.read()
        artifact_id = f"legacy_watch.notice:{octopus_content_hash(manifest_bytes)}"
        seq = runtime.append({"kind": "InterfaceArrival", "from": {"Interface": "world_legacy_watch"}, "to": {"Interface": "system_worker_operator_task_x"}, "artifact": {"id": artifact_id, "content": octopus_content_hash(manifest_bytes)}, "invocation_metadata": {}})
        runtime.stop()
        runtime.start()
        runtime.wait_cursor()
        runtime.expect_no_frame()
        self.assertEqual((runtime.fired()["status"], runtime.fired()["admitted_seq"]), ("admitted", seq))

    def test_after_admission_a_restart_fires_nothing_and_needs_its_arrival_in_the_ledger(self):
        runtime = self.runtime
        runtime.start()
        runtime.letter("Notice", "He is deceased.")
        runtime.admit(runtime.expect_frame("arrival"))
        runtime.wait_cursor()
        fired = runtime.fired()
        runtime.stop()
        runtime.letter("Скончался", "Another matching letter.")
        runtime.start()
        runtime.wait_cursor()
        runtime.expect_no_frame()
        self.assertEqual(runtime.fired(), fired)
        runtime.stop()
        path = os.path.join(runtime.var, "fired.json")
        with open(path, "w") as handle:
            json.dump({**fired, "admitted_seq": fired["admitted_seq"] + 1000}, handle)
        runtime.start(ready=False)
        self.assertEqual(runtime.process.wait(timeout=10), 1)
        time.sleep(0.1)
        self.assertIn("holds no such arrival", "".join(runtime.stderr))

    def test_a_transient_refusal_hands_the_same_notice_again_until_admitted(self):
        runtime = self.runtime
        runtime.start()
        runtime.letter("Digital legacy", "Request received.")
        first = runtime.expect_frame("arrival")
        runtime.refuse(first, "registry_unstable", {})
        second = runtime.expect_frame("arrival")
        self.assertEqual({**second, "request_id": first["request_id"]}, first)
        with open(second["artifact_manifest_path"], "rb") as handle:
            manifest_bytes = handle.read()
        artifact_id = f"legacy_watch.notice:{octopus_content_hash(manifest_bytes)}"
        seq = runtime.append({"kind": "InterfaceArrival", "from": {"Interface": "world_legacy_watch"}, "to": {"Interface": "system_worker_operator_task_x"}, "artifact": {"id": artifact_id, "content": octopus_content_hash(manifest_bytes)}, "invocation_metadata": {}})
        runtime.refuse(second, "duplicate_evidence", {"artifact_id": artifact_id, "message": "arrival evidence already exists in the movement ledger"})
        runtime.wait_cursor()
        runtime.expect_no_frame()
        self.assertEqual((runtime.fired()["status"], runtime.fired()["admitted_seq"]), ("admitted", seq))

    def test_a_permanent_refusal_spends_the_trigger_and_the_life_stays_up(self):
        runtime = self.runtime
        runtime.start()
        runtime.letter("Legacy Contact request", "Please confirm.")
        runtime.refuse(runtime.expect_frame("arrival"), "no_binding", {"message": "no checkpoint pins world_legacy_watch"})
        runtime.letter("Deceased", "A later matching letter.")
        runtime.wait_cursor()
        runtime.expect_no_frame()
        fired = runtime.fired()
        self.assertEqual((fired["status"], fired["admitted_seq"], fired["refusal"]["reason"]), ("refused", None, "no_binding"))
        runtime.stop()
        runtime.start()
        runtime.wait_cursor()
        runtime.expect_no_frame()
        self.assertEqual(runtime.fired(), fired)

    def test_malformed_rows_and_hash_mismatches_stop_the_life(self):
        runtime = self.runtime
        runtime.start()
        runtime.letter("Plain", "Nothing.")
        runtime.wait_cursor()
        digest = runtime.put_blob(b"{}")
        with open(os.path.join(runtime.blobs, digest), "wb") as handle:
            handle.write(b'{"tampered":true}')
        runtime.append({
            "kind": "InterfaceOutbound",
            "from": {"Interface": "world_email"},
            "to": {"Interface": "world_legacy_watch"},
            "artifact": {"id": f"email.event:{digest}", "content": digest},
            "invocation_metadata": {"runtime": {"run": "r", "ee": "e", "worker": "w", "checkpoint": "c", "interface": "world_legacy_watch", "epoch": 0}},
        })
        self.assertEqual(runtime.process.wait(timeout=10), 1)
        time.sleep(0.1)
        self.assertIn("content hash", "".join(runtime.stderr))
        self.assertIsNone(runtime.fired())

        other = FakeRuntime(os.path.join(self.directory, "second"))
        other.start()
        try:
            other.append_raw(b"{not json\n")
            self.assertEqual(other.process.wait(timeout=10), 1)
            time.sleep(0.1)
            self.assertIn("not JSON", "".join(other.stderr))
        finally:
            other.stop()


FAKE_PLANNER = r'''#!{python}
import json, os, sys
sys.dont_write_bytecode = True
sys.path.insert(0, {sensor!r})
from octopus_content_hash import octopus_content_hash
home = {home!r}
request = json.loads(sys.stdin.read())
interfaces = os.path.join(home, "registry", "interface")
with open(os.path.join(interfaces, "world_email", "config.json")) as handle:
    authz = json.load(handle)["authz"]
observed = {{
    "sensor_files": sorted(os.listdir(os.path.join(interfaces, "world_legacy_watch"))),
    "worker_files": sorted(os.listdir(os.path.join(interfaces, "system_worker_legacy_relay"))),
    "email_open": authz["allow_all_senders"] is True and authz["ignore_automated"] is False,
    "relay_present": os.path.exists(os.path.join(home, "state", "runs", "run_legacy_watch_relay.json")),
}}
with open({observation!r}, "w") as handle:
    json.dump({{"request": request, "observed": observed}}, handle)
chat = request["chat_id"]
trigger = [row for row in request["external_inputs"] if row["checkpoint_id"] == "cp_legacy_watch"][0]
profile = {{"ee_id": request["ee_id"], "worker_choice_interface": request["worker_interface_id"], "door_host_slots": request["door_host_slots"]}}
profile.update({{field: request[field] for field in ("external_inputs", "external_relays", "model_outputs", "context_artifact_bindings", "payload_artifact_bindings", "selected_worker_environment_kinds", "selected_tools") if request[field]}})
run_id = "run_operator_chat_" + chat
client = "world_operator_chat_" + chat
model = "world_provider_codex_" + chat
worker = "system_worker_operator_task_" + chat
shared = os.path.join(home, "registry", "artifact", "shared", "artifact.json")
with open(shared, "rb") as handle:
    shared_hash = octopus_content_hash(handle.read())
sidecar = {{"schema_version": 1, "workspace": "octopus_os", "interface": client, "owner_interface": client, "component": "client", "surface_role": "task", "provider_choice_interface": request["provider_interface_id"], "worker_choice_interface": request["worker_interface_id"]}}
run = {{"id": run_id, "ee": request["ee_id"], "worker": worker, "trace": "trace:" + run_id, "status": "idle", "interface_state": {{}},
       "checkpoints": [{{"id": "cp_legacy_watch", "run": run_id, "trace": "t", "epoch": 0,
           "interface_binding": {{"interface": "world_legacy_watch", "config": {{"artifact_policy": {{"mode": "allow", "names": trigger["artifact_names"], "namespaces": []}}}}}},
           "outbound_interface_bindings": [{{"interface": model, "config": {{"artifact_policy": {{"mode": "all"}}}}}}], "artifact_bindings": [], "required_verified_artifacts": []}}],
       "active_checkpoints": ["cp_legacy_watch"]}}
plan = {{
    "run_id": run_id, "chat_interface_id": client, "model_interface_id": model, "worker_interface_id": worker,
    "client_surface_role": "task", "expected_before_hash_algorithm": "blake3", "removals": [], "authoring_profile": profile,
    "interface_writes": [
        {{"path": os.path.join(interfaces, client, "interface.json"), "content": json.dumps({{"id": client}}) + "\n", "tag": "interface:" + client, "expected_before_hash": None}},
        {{"path": os.path.join(interfaces, client, "octopus_os.json"), "content": json.dumps(sidecar, indent=2) + "\n", "tag": "octopus-os:" + client, "expected_before_hash": None}},
        {{"path": shared, "content": json.dumps({{"id": "shared", "version": 2}}) + "\n", "tag": "artifact:shared", "expected_before_hash": shared_hash}},
    ],
    "run_writes": [
        {{"path": os.path.join(home, "state", "runs", "run_writer_" + chat + ".json"), "content": json.dumps({{"id": "run_writer_" + chat}}) + "\n", "tag": "run:writer", "expected_before_hash": None}},
    ],
    "run_record_content": json.dumps(run),
}}
print(json.dumps(plan))
'''

FAKE_RUNTIME = r'''#!{python}
import os, sys
if sys.argv[1:] != ["project-ee-policies"] or os.environ.get("OCTOPUS_HOME") != {home!r}:
    sys.exit("unexpected runtime call")
with open({projection!r}) as handle:
    sys.stdout.write(handle.read())
'''
TOOLS = {"file.read": ("system_room", ["system_room"]), "workspace.exec": ("system_workspace_env", ["filesystem_workspace"]), "memory.save": ("system_memory", ["system_room"]), "git.status": ("system_git", ["git_worktree"])}
KINDS = {"system_room": ["run_records_root"], "filesystem_workspace": ["workspace_root", "host_filesystem_root"], "git_worktree": ["repo_root"]}
CEILING_INTERFACES = ["system_room", "system_workspace_env", "system_memory", "world_email", "world_notes", "world_draft", "world_provider_codex"]
PROFILE = {
    "ee_id": "ee_existence", "worker_choice_interface": "system_worker_operator_task",
    "door_host_slots": {"system_workspace_env": {"tool_cache_root": "/opt/tool-cache"}, "system_git": {"repo_root": "/opt/repo"}},
    "context_artifact_bindings": [{"id": "artifact-worker-role-operator-task"}],
    "selected_worker_environment_kinds": ["system_room", "git_worktree"],
    "selected_tools": [{"tool": "file.read", "interface": "system_room", "worker_environment_kind": "system_room"}],
}


class AuthorTest(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.mkdtemp(prefix="legacy-watch-author-")
        self.home = os.path.realpath(os.path.join(self.directory, "home"))
        interfaces = os.path.join(self.home, "registry", "interface")
        email = os.path.join(interfaces, "world_email")
        os.makedirs(email)
        source = os.path.join(REPO, "registry", "interface", "world_email")
        for name in ("interface.json", "life.json", "config.json", "application.json"):
            shutil.copyfile(os.path.join(source, name), os.path.join(email, name))
        shutil.copytree(os.path.join(source, "operations"), os.path.join(email, "operations"))
        self.write(os.path.join(interfaces, "world_operator_chat_home1", "octopus_os.json"), {"schema_version": 1, "workspace": "octopus_os", "interface": "world_operator_chat_home1", "owner_interface": "world_operator_chat_home1", "component": "client", "surface_role": "home", "provider_choice_interface": "world_provider_codex", "worker_choice_interface": "system_worker_operator_home", "authoring_profile": {"ee_id": "ee_existence", "worker_choice_interface": "system_worker_operator_home", "door_host_slots": {}}, "task_authoring_profiles": [PROFILE], "presentation": {"name": "Home", "projects": []}})
        self.write(os.path.join(interfaces, "system_worker_operator_task", "policy.json"), {"id": "system_worker_operator_task", "ceiling": {"interfaces": CEILING_INTERFACES, "tools": ["file.read", "workspace.exec", "memory.save"], "wek": ["system_room", "filesystem_workspace"]}})
        for tool, (owner, kinds) in TOOLS.items():
            self.write(os.path.join(self.home, "registry", "tool", tool, "tool.json"), {"id": tool, "interface": owner, "supported_worker_environment_kinds": kinds})
        for kind, roots in KINDS.items():
            self.write(os.path.join(self.home, "registry", "worker_environment_kind", kind, "worker_environment_kind.json"), {"worker_environment_kind_id": kind, "boundary_defaults": {"read_root_refs": [{"path": root} for root in roots], "write_root_refs": []}})
        for interface_id in ("system_room", "system_workspace_env", "system_memory", "system_git"):
            self.write(os.path.join(interfaces, interface_id, "interface.json"), {"id": interface_id})
        catalog = {"operation_catalog_dir": "operations", "operation_catalog_label": "x.operation_catalog", "operation_request_label": "x.operation.request"}
        applications = {
            "system_memory": ("plugin", [], [{"tool": "memory.save", "interface": "system_memory", "worker_environment_kind": "system_room"}]),
            "world_notes": ("connector", [{"artifact_id": "artifact-application-notes-context"}], []),
            "world_draft": ("connector", [], []),
        }
        for application_id, (category, bindings, tools) in applications.items():
            if application_id != "system_memory":
                self.write(os.path.join(interfaces, application_id, "interface.json"), {"id": application_id, "interface_driver_config": {"private_config": catalog}})
            self.write(os.path.join(interfaces, application_id, "application.json"), {"schema_version": 3, "id": application_id, "category": category, "interfaces": [application_id], "artifact_bindings": bindings, "task_capabilities": {"tools": tools}})
        for application_id in ("system_memory", "world_notes", "world_draft", "world_email"):
            self.write(os.path.join(self.home, "client", "package-state", "receipts", f"{application_id}.json"), {"package_id": application_id})
        self.write(os.path.join(self.home, "registry", "artifact", "artifact-operation-catalog-world_notes", "artifact.json"), {"id": "artifact-operation-catalog-world_notes", "interface": "world_notes", "operations": [{"name": "notes.list"}]})
        self.write(os.path.join(self.home, "registry", "artifact", "artifact-operation-catalog-world_email", "artifact.json"), {"id": "artifact-operation-catalog-world_email", "interface": "world_email", "task_input_labels": ["email.event"], "operations": [{"name": "email.reply"}]})
        self.write(os.path.join(self.home, "host.json"), {"host_manifest_version": 1, "root_refs": {"workspace_root": "/opt/workspace"}})
        self.projection = os.path.join(self.directory, "projection.json")
        allowed = {"allowed_interfaces": CEILING_INTERFACES + ["system_git", "world_legacy_watch", "system_worker_legacy_relay"], "allowed_tools": list(TOOLS), "allowed_worker_environment_kinds": list(KINDS)}
        self.write(self.projection, {"environments": [{"ee_id": "ee_existence", "effective_policy": allowed, "policy_ceiling": allowed, "workers": [{"worker_interface_id": "system_worker_operator_task", "interface_policy": {"world_provider_codex": {"models": ["model-one"]}}}]}], "issues": []})
        self.write(os.path.join(interfaces, "world_provider_codex", "interface.json"), {"id": "world_provider_codex"})
        self.write(os.path.join(interfaces, "world_provider_codex_home1", "interface.json"), {"id": "world_provider_codex_home1", "interface_driver_config": {"private_config": {"model": "model-one", "reasoning_effort": "medium"}}})
        self.write(os.path.join(self.home, "registry", "artifact", "shared", "artifact.json"), {"id": "shared", "version": 1})
        self.write(os.path.join(self.home, "state", "ee_instances", "ee_existence.json"), {"ee_id": "ee_existence", "template_name": "existence", "parent_ee_id": None, "goal": "root"})
        self.write(os.path.join(self.home, "registry", "ee_template", "existence", "ee_template.json"), {"id": "existence", "allowed_interfaces": None})
        self.write(os.path.join(self.home, "state", "runs", "run_operator_chat_home1.json"), {"id": "run_operator_chat_home1", "ee": "ee_existence", "worker": "system_worker_operator_home_home1", "trace": "t", "status": "idle", "interface_state": {"world_operator_chat_home1": {"client_surface_role": "home", "provider_choice_interface": "world_provider_codex", "provider_runtime_interface": "world_provider_codex_home1"}}, "checkpoints": [], "active_checkpoints": []})
        self.observation = os.path.join(self.directory, "observation.json")
        self.planner = os.path.join(self.directory, "planner.py")
        with open(self.planner, "w") as handle:
            handle.write(FAKE_PLANNER.format(python=sys.executable, sensor=SENSOR_DIR, home=self.home, observation=self.observation))
        os.chmod(self.planner, 0o755)
        self.runtime = os.path.join(self.directory, "runtime.py")
        with open(self.runtime, "w") as handle:
            handle.write(FAKE_RUNTIME.format(python=sys.executable, home=self.home, projection=self.projection))
        os.chmod(self.runtime, 0o755)

    def tearDown(self):
        shutil.rmtree(self.directory)

    def write(self, path, value):
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "w") as handle:
            json.dump(value, handle, indent=2)

    def author(self, *extra, planner=None):
        return subprocess.run(
            [sys.executable, AUTHOR, "--home", self.home, "--planner", planner or self.planner, "--runtime", self.runtime, "--title", "Watch legacy mail", *extra],
            capture_output=True, text=True, env={**os.environ, "PYTHONDONTWRITEBYTECODE": "1"}, timeout=60,
        )

    def snapshot(self):
        result = {}
        for root, directories, files in os.walk(self.home):
            for name in directories:
                result[os.path.join(root, name)] = None
            for name in files:
                path = os.path.join(root, name)
                with open(path, "rb") as handle:
                    result[path] = handle.read()
        return result

    def turn_starting(self, interface_id):
        with open(os.path.join(self.home, "registry", "interface", interface_id, "interface.json")) as handle:
            private = json.load(handle).get("interface_driver_config", {}).get("private_config", {})
        catalog = os.path.join(self.home, "registry", "artifact", f"artifact-operation-catalog-{interface_id}", "artifact.json")
        if "operation_catalog_dir" not in private or not os.path.isfile(catalog):
            return True
        with open(catalog) as handle:
            return bool(json.load(handle).get("task_input_labels"))

    def armed_at(self):
        with open(os.path.join(self.home, "registry", "interface", "world_legacy_watch", "armed.json")) as handle:
            return json.load(handle)["armed_at"]

    def test_authoring_order_contents_and_refusal_without_replace(self):
        with open(os.path.join(self.home, "registry", "interface", "world_email", "config.json")) as handle:
            text_before = handle.read()
        config_before = json.loads(text_before)
        completed = self.author()
        self.assertEqual(completed.returncode, 0, completed.stderr)
        with open(self.observation) as handle:
            observation = json.load(handle)
        request, observed = observation["request"], observation["observed"]
        chat = request["chat_id"]
        interfaces = os.path.join(self.home, "registry", "interface")
        runs = os.path.join(self.home, "state", "runs")
        wrote = [line[len("wrote "):] for line in completed.stdout.splitlines() if line.startswith("wrote ")]
        self.assertEqual(wrote, [
            f"folder {os.path.join(interfaces, 'world_legacy_watch')}",
            f"folder {os.path.join(interfaces, 'system_worker_legacy_relay')}",
            os.path.join(interfaces, f"world_operator_chat_{chat}", "interface.json"),
            os.path.join(interfaces, f"world_operator_chat_{chat}", "octopus_os.json"),
            os.path.join(self.home, "registry", "artifact", "shared", "artifact.json"),
            os.path.join(runs, f"run_writer_{chat}.json"),
            os.path.join(runs, f"run_operator_chat_{chat}.json"),
            os.path.join(runs, "run_legacy_watch_relay.json"),
        ])
        self.assertIn("senders: only the mailbox's own address and the Allowed senders set in Email reach the trigger, automated mail is dropped", completed.stdout)
        self.assertEqual(observed, {
            "sensor_files": ["armed.json", "interface.json", "legacy_watch_life.py", "life.json", "octopus_content_hash.py", "watch.json"],
            "worker_files": ["interface.json", "policy.json"],
            "email_open": False,
            "relay_present": False,
        })
        self.assertEqual([path for path in self.snapshot() if os.path.basename(path).startswith(".")], [])
        request.pop("conversation")
        for key in ("registry_root", "ee_instances_root", "ee_policy_root", "run_records_root", "interface_private_state_root", "chat_id", "cycle"):
            self.assertTrue(request.pop(key))
        self.assertEqual(request, {
            "write_hash_algorithm": "blake3", "provider_interface_id": "world_provider_codex", "model": "model-one", "reasoning_effort": "medium", "service_tier": None,
            "ee_id": "ee_existence", "worker_interface_id": "system_worker_operator_task", "client_surface_role": "task",
            "door_host_slots": {"system_workspace_env": {"tool_cache_root": "/opt/tool-cache"}},
            "external_inputs": [
                {"checkpoint_id": "cp_app_world_notes_input_1", "interface_id": "world_notes"},
                {"checkpoint_id": "cp_legacy_watch", "interface_id": "world_legacy_watch", "artifact_names": ["legacy_watch.notice"]},
            ],
            "external_relays": [], "model_outputs": [],
            "context_artifact_bindings": [{"id": "artifact-worker-role-operator-task"}, {"id": "artifact-application-notes-context"}],
            "payload_artifact_bindings": [],
            "selected_worker_environment_kinds": ["filesystem_workspace", "system_room"],
            "selected_tools": [
                {"tool": "file.read", "interface": "system_room", "worker_environment_kind": "system_room"},
                {"tool": "memory.save", "interface": "system_memory", "worker_environment_kind": "system_room"},
                {"tool": "workspace.exec", "interface": "system_workspace_env", "worker_environment_kind": "filesystem_workspace"},
            ],
            "publications": [], "existing_run_record": None, "retain_current_capabilities": False,
        })
        for row in request["external_inputs"]:
            if row["checkpoint_id"] != "cp_legacy_watch":
                self.assertFalse(self.turn_starting(row["interface_id"]), row)
        self.assertNotIn("/", [root for slots in request["door_host_slots"].values() for root in slots.values()])
        self.assertIn("task access: the root task profile's own door slots", completed.stdout)
        self.assertIn("applications: world_notes", completed.stdout)
        self.assertIn("left out system_memory: its input from system_memory accepts every artifact", completed.stdout)
        self.assertIn("left out world_draft: world_draft is not connected", completed.stdout)
        self.assertIn("left out world_email: its task input email.event would wake the task", completed.stdout)
        self.assertIn("selects git_worktree, which this Home does not offer", completed.stdout)
        with open(os.path.join(interfaces, f"world_operator_chat_{chat}", "octopus_os.json")) as handle:
            self.assertEqual(json.load(handle)["presentation"], {"name": "Watch legacy mail", "projects": []})
        with open(os.path.join(interfaces, "world_email", "config.json")) as handle:
            text_after = handle.read()
        self.assertEqual(text_after, text_before)
        with open(os.path.join(runs, "run_legacy_watch_relay.json")) as handle:
            relay = json.load(handle)
        self.assertEqual((relay["ee"], relay["worker"], relay["active_checkpoints"]), ("ee_existence", "system_worker_legacy_relay", ["cp_legacy_watch_email"]))
        checkpoint = relay["checkpoints"][0]
        self.assertEqual(checkpoint["interface_binding"], {"interface": "world_email", "config": {"artifact_policy": {"mode": "allow", "names": ["email.event", "email.operation_catalog"], "namespaces": []}}})
        self.assertEqual(checkpoint["outbound_interface_bindings"], [{"interface": "world_legacy_watch", "config": {"artifact_policy": {"mode": "allow", "names": ["email.event"], "namespaces": []}}}])

        before = self.snapshot()
        refused = self.author()
        self.assertEqual(refused.returncode, 2)
        self.assertIn("already exists; pass --replace", refused.stderr)
        self.assertEqual(self.snapshot(), before)

        armed_at = self.armed_at()
        replaced = self.author("--replace")
        self.assertEqual(replaced.returncode, 0, replaced.stderr)
        self.assertEqual(self.armed_at(), armed_at)
        self.assertIn(f"kept the receiving task run_operator_chat_{chat}", replaced.stdout)

        stale = "2020-01-01T00:00:00+00:00"
        self.write(os.path.join(interfaces, "world_legacy_watch", "armed.json"), {"armed_at": stale})
        os.remove(os.path.join(runs, f"run_operator_chat_{chat}.json"))
        reauthored = self.author("--replace")
        self.assertEqual(reauthored.returncode, 0, reauthored.stderr)
        self.assertNotIn("kept the receiving task", reauthored.stdout)
        self.assertGreater(self.armed_at(), stale)

    def test_a_missing_task_profile_a_narrowed_ee_chain_or_a_failing_planner_leaves_the_home_unchanged(self):
        sidecar_path = os.path.join(self.home, "registry", "interface", "world_operator_chat_home1", "octopus_os.json")
        with open(sidecar_path) as handle:
            sidecar = json.load(handle)
        self.write(sidecar_path, {**sidecar, "task_authoring_profiles": []})
        before = self.snapshot()
        refused = self.author()
        self.assertEqual(refused.returncode, 2)
        self.assertIn("has no task authoring profile for EE ee_existence and worker system_worker_operator_task", refused.stderr)
        self.assertEqual(self.snapshot(), before)
        self.write(sidecar_path, sidecar)

        instance = os.path.join(self.home, "state", "ee_instances", "ee_existence.json")
        with open(instance) as handle:
            narrowed = json.load(handle)
        narrowed["policy_narrowing"] = {"allowed_interfaces": ["world_email", "world_legacy_watch"]}
        self.write(instance, narrowed)
        before = self.snapshot()
        refused = self.author()
        self.assertEqual(refused.returncode, 2)
        self.assertIn("system_worker_legacy_relay (by instance ee_existence)", refused.stderr)
        self.assertEqual(self.snapshot(), before)

        del narrowed["policy_narrowing"]
        self.write(instance, narrowed)
        before = self.snapshot()
        failing = os.path.join(self.directory, "failing-planner.sh")
        with open(failing, "w") as handle:
            handle.write("#!/bin/sh\necho planner refused >&2\nexit 1\n")
        os.chmod(failing, 0o755)
        refused = self.author(planner=failing)
        self.assertEqual(refused.returncode, 2)
        self.assertIn("planner refused", refused.stderr)
        self.assertIn("every earlier step of this run was undone", refused.stderr)
        self.assertEqual(self.snapshot(), before)

    def test_state_left_by_an_earlier_install_is_refused_until_rearmed(self):
        state = os.path.join(self.home, "interfaces", "world_legacy_watch")
        os.makedirs(os.path.join(state, "payloads"), mode=0o700)
        self.write(os.path.join(state, "fired.json"), {"status": "admitted", "admitted_seq": 41})
        before = self.snapshot()
        refused = self.author()
        self.assertEqual(refused.returncode, 2)
        self.assertIn("pass --rearm", refused.stderr)
        self.assertIn("admitted at ledger seq 41", refused.stderr)
        self.assertEqual(self.snapshot(), before)
        config_path = os.path.join(self.home, "registry", "interface", "world_email", "config.json")
        with open(config_path) as handle:
            text_before = handle.read()
        config_before = json.loads(text_before)
        rearmed = self.author("--rearm", "--full-access", "--any-sender")
        self.assertEqual(rearmed.returncode, 0, rearmed.stderr)
        self.assertIn("full machine access", rearmed.stdout)
        self.assertIn("now admits every sender", rearmed.stdout)
        self.assertIn("senders: every sender reaches the trigger, automated mail included", rearmed.stdout)
        with open(config_path) as handle:
            text_after = handle.read()
        config_before["authz"].update({"allow_all_senders": True, "ignore_automated": False})
        self.assertEqual(json.loads(text_after), config_before)
        self.assertEqual(text_after.replace('"allow_all_senders": true', '"allow_all_senders": false').replace('"ignore_automated": false', '"ignore_automated": true'), text_before)
        with open(self.observation) as handle:
            slots = json.load(handle)["request"]["door_host_slots"]
        self.assertEqual(slots, {"system_workspace_env": {"tool_cache_root": "/opt/tool-cache", "host_filesystem_root": "/", "workspace_root": "/"}})
        self.assertEqual(sorted(os.listdir(state)), [])
        self.assertTrue(self.armed_at())


if __name__ == "__main__":
    unittest.main()
