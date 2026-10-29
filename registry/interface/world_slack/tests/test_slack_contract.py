import hashlib
import importlib
import json
import os
import socket
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock


ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))

import slack_contract
from slack_wire import SlackApi
from slack_wire import SlackWireError
from slack_wire import WebSocket
from slack_wire import validate_proxy_url


def interface_environment():
    with open(ROOT / "interface.json", "r", encoding="utf-8") as handle:
        return json.load(handle)["interface_driver_config"]["private_config"]["environment"]


class SlackContractTests(unittest.TestCase):
    def setUp(self):
        with open(ROOT / "config.json", "r", encoding="utf-8") as handle:
            self.config = json.load(handle)
        self.config["environment"] = interface_environment()

    def test_config_and_environment_are_strict(self):
        slack_contract.validate_config(self.config)
        extra = json.loads(json.dumps(self.config))
        extra["routing"] = {}
        with self.assertRaisesRegex(ValueError, "unknown keys"):
            slack_contract.validate_config(extra)
        shared = json.loads(json.dumps(self.config))
        shared["environment"]["app_token_env"] = shared["environment"]["bot_token_env"]
        with self.assertRaisesRegex(ValueError, "distinct"):
            slack_contract.validate_config(shared)

    def test_rich_text_preserves_quotes_lists_links_and_mentions(self):
        blocks = [
            {
                "type": "rich_text",
                "elements": [
                    {
                        "type": "rich_text_quote",
                        "elements": [
                            {
                                "type": "rich_text_section",
                                "elements": [
                                    {"type": "text", "text": "See "},
                                    {"type": "link", "text": "proof", "url": "https://example.com"},
                                    {"type": "user", "user_id": "U12345678"},
                                ],
                            }
                        ],
                    },
                    {
                        "type": "rich_text_list",
                        "style": "ordered",
                        "elements": [
                            {"type": "rich_text_section", "elements": [{"type": "text", "text": "one"}]},
                            {"type": "rich_text_section", "elements": [{"type": "text", "text": "two"}]},
                        ],
                    },
                ],
            }
        ]
        rendered = slack_contract.rich_text(blocks)
        self.assertIn("> See proof (https://example.com)<@U12345678>", rendered)
        self.assertIn("1. one", rendered)
        self.assertIn("2. two", rendered)

    def test_coordinates_cover_reactions_edits_and_interactions(self):
        reaction = {
            "type": "events_api",
            "payload": {
                "team_id": "T12345678",
                "event": {"type": "reaction_added", "item": {"channel": "C12345678", "ts": "1720000000.000100"}},
            },
        }
        self.assertEqual(
            slack_contract.event_coordinates(reaction),
            ("T12345678", "C12345678", None, "1720000000.000100"),
        )
        changed = {
            "type": "events_api",
            "payload": {
                "team_id": "T12345678",
                "event": {
                    "type": "message",
                    "subtype": "message_changed",
                    "channel": "C12345678",
                    "message": {"ts": "1720000001.000200", "thread_ts": "1720000000.000100"},
                },
            },
        }
        self.assertEqual(slack_contract.event_coordinates(changed)[2:], ("1720000000.000100", "1720000001.000200"))
        interactive = {
            "type": "interactive",
            "payload": {
                "team": {"id": "T12345678"},
                "container": {"channel_id": "C12345678", "thread_ts": "1720000000.000100", "message_ts": "1720000001.000200"},
            },
        }
        self.assertEqual(slack_contract.event_coordinates(interactive), ("T12345678", "C12345678", "1720000000.000100", "1720000001.000200"))

    def test_operation_vocabulary_rejects_unknown_effects(self):
        operation, args = slack_contract.normalize_operation(
            {"operation": "view.open", "args": {"trigger_handle": "a" * 64, "view": {"type": "modal"}}},
            self.config,
        )
        self.assertEqual(operation, "view.open")
        self.assertEqual(args["view"]["type"], "modal")
        with self.assertRaisesRegex(ValueError, "not supported"):
            slack_contract.normalize_operation({"operation": "admin.users.remove", "args": {}}, self.config)

    def test_markdown_is_rendered_without_touching_code(self):
        rendered = slack_contract.slack_mrkdwn("# Title\n**bold** [proof](https://example.com) `**code**`")
        self.assertEqual(rendered, "*Title*\n*bold* <https://example.com|proof> `**code**`")

    def test_websocket_reader_handles_ping_and_fragmentation(self):
        client, server = socket.socketpair()
        wire = WebSocket("wss://example.test/socket", 1000, 4096)
        wire.sock = client
        payload = json.dumps({"type": "events_api", "envelope_id": "e1"}).encode("utf-8")
        first = bytes([0x01, 2]) + payload[:2]
        ping = bytes([0x89, 1]) + b"p"
        second = bytes([0x80, len(payload) - 2]) + payload[2:]
        server.sendall(first + ping + second)
        self.assertEqual(wire.receive_json()["envelope_id"], "e1")
        pong = server.recv(1024)
        self.assertEqual(pong[0] & 0x0F, 10)
        wire.sock = None
        client.close()
        server.close()

    def test_websocket_rejects_fragmented_control_frame(self):
        client, server = socket.socketpair()
        wire = WebSocket("wss://example.test/socket", 1000, 4096)
        wire.sock = client
        server.sendall(bytes([0x09, 1]) + b"p")
        with self.assertRaisesRegex(SlackWireError, "control frame"):
            wire.receive_json()
        wire.sock = None
        client.close()
        server.close()

    def test_proxy_is_explicit_and_remote_plaintext_is_refused(self):
        with self.assertRaisesRegex(ValueError, "must use HTTPS"):
            validate_proxy_url("http://proxy.example:8080")
        with self.assertRaisesRegex(ValueError, "invalid characters"):
            validate_proxy_url(" https://proxy.example:8443")
        with self.assertRaisesRegex(ValueError, "credentials require"):
            validate_proxy_url("https://user@proxy.example:8443")
        with mock.patch.dict(os.environ, {"HTTP_PROXY": "http://ambient.invalid:8080", "HTTPS_PROXY": "http://ambient.invalid:8080", "OCTOPUS_SLACK_PROXY": ""}, clear=False):
            api = SlackApi(self.config, ["xoxb-" + "a" * 32], "xapp-" + "b" * 32)
            self.assertEqual(api.proxy_url, "")

    def test_remote_refusal_body_is_reduced_to_digest_evidence(self):
        api = SlackApi(self.config, ["xoxb-" + "a" * 32], "xapp-" + "b" * 32)
        body = json.dumps({"ok": False, "error": "invalid_auth", "secret": "remote-private-body"}).encode("utf-8")
        api._request = mock.Mock(return_value=(200, {}, body))
        with self.assertRaises(SlackWireError) as raised:
            api.call("auth.test")
        error = raised.exception
        self.assertEqual(error.details["body_bytes"], len(body))
        self.assertEqual(error.details["body_sha256"], hashlib.sha256(body).hexdigest())
        self.assertNotIn("remote-private-body", str(error))
        self.assertNotIn("remote-private-body", json.dumps(error.details))

    def test_external_ids_become_safe_stable_artifact_keys(self):
        first = slack_contract.envelope_key({"type": "events_api", "payload": {"event_id": "../../same"}})
        second = slack_contract.envelope_key({"type": "events_api", "payload": {"event_id": "../../same"}})
        self.assertEqual(first, second)
        self.assertRegex(first, r"^event-[0-9a-f]{64}$")
        self.assertRegex(slack_contract.envelope_key({"envelope_id": "../outside", "payload": {}}), r"^envelope-[0-9a-f]{64}$")


class SlackLifeTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        root = Path(self.temp.name)
        for name in ("blobs", "var"):
            (root / name).mkdir()
        self.environment = mock.patch.dict(
            os.environ,
            {
                "OCTOPUS_INTERFACE_ID": "world_slack",
                "OCTOPUS_INTERFACE_DIR": str(ROOT),
                "OCTOPUS_VAR_DIR": str(root / "var"),
                "OCTOPUS_MOVEMENTS_JSONL": str(root / "movements.jsonl"),
                "OCTOPUS_BLOBS_DIR": str(root / "blobs"),
                "OCTOPUS_LEDGER_INDEX_SQLITE": str(root / "ledger-index.sqlite"),
                "SLACK_ALLOWED_USERS": "U12345678",
                "SLACK_ALLOWED_CHANNELS": "",
                "SLACK_HOME_CHANNEL": "",
            },
        )
        self.environment.start()
        sys.modules.pop("slack_life", None)
        self.life = importlib.import_module("slack_life")
        self.config = self.life.load_config()
        self.life._CONFIG = self.config

    def tearDown(self):
        self.environment.stop()
        self.temp.cleanup()

    def direct_message(self, text="hello"):
        return {
            "type": "events_api",
            "envelope_id": "envelope-1",
            "payload": {
                "team_id": "T12345678",
                "event_id": "Ev12345678",
                "event": {"type": "message", "channel": "D12345678", "user": "U12345678", "ts": "1720000000.000100", "text": text},
            },
        }

    def test_admitted_message_becomes_one_slack_event(self):
        _, record = self.life.persist_envelope(self.config, self.direct_message("deploy status?"))
        handed = []

        def hand(manifest_path, evidence=None):
            manifest = json.loads(Path(manifest_path).read_text(encoding="utf-8"))
            handed.append((manifest["label"], json.loads(Path(manifest["payload_path"]).read_text(encoding="utf-8")), evidence))
            return 41

        with mock.patch.object(self.life, "hand_artifact", side_effect=hand):
            self.assertEqual(self.life.process_pending_event(object(), self.config, record), 41)
            self.assertIsNone(self.life.process_pending_event(object(), self.config, record))
        self.assertEqual(len(handed), 1)
        label, payload, evidence = handed[0]
        self.assertEqual(label, "slack.event")
        self.assertEqual((payload["channel_id"], payload["user_id"], payload["message"]["text"], payload["message_ts"]), ("D12345678", "U12345678", "deploy status?", "1720000000.000100"))
        self.assertNotIn("payload", payload)
        self.assertEqual(evidence[0]["label"], "slack.arrival.idempotency")
        self.assertFalse(os.listdir(self.life.PENDING_DIR))

    def test_unallowed_member_is_not_handed_to_tasks(self):
        envelope = self.direct_message()
        envelope["payload"]["event"]["user"] = "U99999999"
        _, record = self.life.persist_envelope(self.config, envelope)
        with mock.patch.object(self.life, "hand_artifact") as hand:
            self.assertIsNone(self.life.process_pending_event(object(), self.config, record))
        hand.assert_not_called()
        self.assertIn(record["key"], self.life.load_state()["processed"])

    def test_reply_without_channel_reaches_the_single_allowed_member(self):
        operation, args = self.life.prepare_output(self.config, "slack.operation", json.dumps({"text": "Build finished"}).encode("utf-8"))
        api = mock.Mock()
        api.call.return_value = {"ok": True, "channel": "D12345678", "ts": "1720000002.000300"}
        result = self.life.execute_operation(api, self.config, operation, args, "a" * 64)
        method, params = api.call.call_args.args
        self.assertEqual((method, params["channel"], params["text"]), ("chat.postMessage", "U12345678", "Build finished"))
        self.assertEqual(result["messages"][0]["channel"], "D12345678")
        self.assertIn("D12345678:1720000002.000300", self.life.load_state()["bot_message_threads"])

    def test_thread_reply_and_foreign_labels(self):
        operation, args = self.life.prepare_output(self.config, "slack.operation", json.dumps({"text": "On it", "channel": "C12345678", "thread_ts": "1720000000.000100"}).encode("utf-8"))
        api = mock.Mock()
        api.call.return_value = {"ok": True, "channel": "C12345678", "ts": "1720000003.000400"}
        self.life.execute_operation(api, self.config, operation, args, "b" * 64)
        posted = [call.args for call in api.call.call_args_list if call.args[0] == "chat.postMessage"][0][1]
        self.assertEqual((posted["channel"], posted["thread_ts"]), ("C12345678", "1720000000.000100"))
        with self.assertRaisesRegex(ValueError, "accepts only 'slack.operation'"):
            self.life.prepare_output(self.config, "interface.artifact.output", b'{"text": "x"}')
        with self.assertRaisesRegex(ValueError, "not a Slack channel id"):
            self.life.execute_operation(api, self.config, "reaction.add", {"channel": "U12345678", "timestamp": "1720000000.000100", "name": "eyes"}, "c" * 64)

    def test_tokens_and_members_fail_closed(self):
        with mock.patch.dict(os.environ, {"SLACK_BOT_TOKEN": "", "SLACK_APP_TOKEN": ""}):
            with self.assertRaisesRegex(ValueError, "bot token is not configured"):
                self.life.resolve_tokens(self.config)
        with mock.patch.dict(os.environ, {"SLACK_BOT_TOKEN": "xoxb-" + "a" * 32, "SLACK_APP_TOKEN": "xapp-" + "b" * 32, "SLACK_ALLOWED_USERS": ""}):
            with self.assertRaisesRegex(ValueError, "allowed members are not configured"):
                self.life.resolve_tokens(self.config)
        with mock.patch.dict(os.environ, {"SLACK_BOT_TOKEN": "xoxb-" + "a" * 32, "SLACK_APP_TOKEN": "xapp-" + "b" * 32}):
            self.assertEqual(self.life.resolve_tokens(self.config), (["xoxb-" + "a" * 32], "xapp-" + "b" * 32))

    def test_mention_gate_becomes_thread_sticky(self):
        state = self.life.load_state()
        state["bot_user_ids"] = {"T12345678": "U87654321"}
        self.life.atomic_json(self.life.STATE_PATH, state)
        mention = {
            "type": "events_api",
            "payload": {
                "team_id": "T12345678",
                "event": {
                    "type": "message",
                    "channel": "C12345678",
                    "user": "U12345678",
                    "ts": "1720000000.000100",
                    "text": "<@U87654321> help",
                },
            },
        }
        self.assertEqual(self.life.should_admit(self.config, mention, self.life.load_state()), (True, "mentioned"))
        reply = json.loads(json.dumps(mention))
        reply["payload"]["event"].update({"thread_ts": "1720000000.000100", "ts": "1720000001.000200", "text": "more"})
        self.assertEqual(self.life.should_admit(self.config, reply, self.life.load_state()), (True, "mentioned_thread"))


    def test_private_file_and_artifact_paths_fail_closed(self):
        root = Path(os.environ["OCTOPUS_VAR_DIR"])
        secret = root / "secret.key"
        secret.write_text("secret\n", encoding="utf-8")
        secret.chmod(0o600)
        with self.life.open_private_read(str(secret)) as handle:
            self.assertEqual(handle.read(), b"secret\n")
        secret.chmod(0o644)
        with self.assertRaisesRegex(PermissionError, "0600"):
            self.life.open_private_read(str(secret))
        link = root / "secret-link.key"
        link.symlink_to(secret)
        with self.assertRaisesRegex(PermissionError, "regular file"):
            self.life.open_private_read(str(link))
        with self.assertRaisesRegex(ValueError, "unsafe characters"):
            self.life.materialize_artifact("slack.event", b"{}", "../escape")


    def test_sanitizer_redacts_nested_secret_encodings(self):
        token = "xoxb-super-secret-value"
        proxy = "https://proxy-user:proxy-pass@proxy.example:8443"
        with mock.patch.dict(os.environ, {"SLACK_BOT_TOKEN": token, "OCTOPUS_SLACK_PROXY": proxy}, clear=False):
            value = self.life.sanitize_external_error({token: [token, "eG94Yi1zdXBlci1zZWNyZXQtdmFsdWU=", proxy]}, self.config)
        encoded = json.dumps(value, sort_keys=True)
        self.assertNotIn(token, encoded)
        self.assertNotIn("proxy-pass", encoded)


    def test_ledger_fallback_bounds_rows_before_json_allocation(self):
        path = Path(os.environ["OCTOPUS_MOVEMENTS_JSONL"])
        path.write_bytes(b"{" + b"x" * 256 + b"}\n")
        self.life._LEDGER_FALLBACK_OFFSET = 0
        self.life._LEDGER_FALLBACK_SIZE = 0
        with self.assertRaisesRegex(RuntimeError, "exceeds 256 bytes"):
            self.life.read_ledger(0, 128, 256)
        path.write_bytes(b'{"seq":0}')
        self.life._LEDGER_FALLBACK_OFFSET = 0
        self.life._LEDGER_FALLBACK_SIZE = 0
        with self.assertRaisesRegex(RuntimeError, "is partial"):
            self.life.read_ledger(0, 128, 256)
        foreign = {
            "seq": 5,
            "kind": "InterfaceOutbound",
            "from": {"Interface": "world_provider_foreign"},
            "to": {"Interface": "world_other"},
            "invocation_metadata": {"runtime": 7},
        }
        legacy = {
            "seq": 6,
            "kind": "InterfaceOutbound",
            "from": {"Run": "run_legacy"},
            "to": {"Interface": "world_slack"},
            "invocation_metadata": {"runtime": 7},
        }
        movement = {
            "seq": 7,
            "kind": "InterfaceOutbound",
            "from": {"Interface": "world_provider_test"},
            "to": {"Interface": "world_slack"},
            "invocation_metadata": {
                "runtime": {
                    "run": "run_slack",
                    "ee": "ee_slack",
                    "worker": "system_worker_operator_task",
                    "checkpoint": "cp_slack",
                    "interface": "world_slack",
                    "epoch": 0,
                }
            },
            "artifact": {"content": [0] * 32},
        }
        path.write_text(
            "".join(json.dumps(row) + "\n" for row in (foreign, legacy, movement)),
            encoding="utf-8",
        )
        self.life._LEDGER_FALLBACK_IDENTITY = None
        self.life._LEDGER_FALLBACK_OFFSET = 0
        self.life._LEDGER_FALLBACK_SIZE = 0
        expected = [(7, [0] * 32, "run_slack")]
        self.assertEqual(self.life.read_ledger(7, 128, 1024), expected)
        self.assertEqual(self.life.read_ledger(7, 128, 1024), expected)
        self.assertEqual(self.life.read_ledger(8, 128, 1024), [])
        malformed = {
            "seq": 8,
            "kind": "InterfaceOutbound",
            "from": {"Interface": "world_provider_test"},
            "to": {"Interface": "world_slack"},
            "invocation_metadata": {"runtime": {"interface": "world_slack"}},
        }
        path.write_text(json.dumps(malformed) + "\n", encoding="utf-8")
        self.life._LEDGER_FALLBACK_IDENTITY = None
        self.life._LEDGER_FALLBACK_OFFSET = 0
        self.life._LEDGER_FALLBACK_SIZE = 0
        with self.assertRaises(ValueError):
            self.life.read_ledger(8, 128, 1024)


if __name__ == "__main__":
    unittest.main()
