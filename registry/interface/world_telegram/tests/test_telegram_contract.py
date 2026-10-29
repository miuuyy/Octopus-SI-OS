import hashlib
import importlib
import json
import os
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock


ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))

import telegram_contract as contract
import telegram_render as render
from telegram_wire import BotApi
from telegram_wire import TelegramWireError
from telegram_wire import parse_proxy


BOT = {"id": 999, "username": "octo_bot", "name": "Octo"}


def base_config():
    config = json.loads((ROOT / "config.json").read_text(encoding="utf-8"))
    config["environment"] = json.loads((ROOT / "interface.json").read_text(encoding="utf-8"))["interface_driver_config"]["private_config"]["environment"]
    return config


def message(text, chat=None, sender=111, message_id=5, **extra):
    value = {"message_id": message_id, "date": 1730000000, "chat": chat or {"id": sender, "type": "private"}, "from": {"id": sender, "first_name": "Ann", "is_bot": False}, "text": text}
    value.update(extra)
    return value


def visible(chunk):
    return "".join(text for text, _ in render.html_segments(chunk))


class ContractTests(unittest.TestCase):
    def setUp(self):
        self.config = base_config()
        self.access = {"allow_all": False, "users": {111}, "chats": set(), "free_chats": set(), "home_chat": None}

    def test_config_and_environment_are_strict(self):
        contract.validate_config(self.config)
        extra = json.loads(json.dumps(self.config))
        extra["routing"] = {}
        with self.assertRaisesRegex(ValueError, "unknown keys"):
            contract.validate_config(extra)
        shared = json.loads(json.dumps(self.config))
        shared["environment"]["home_chat_env"] = shared["environment"]["bot_token_env"]
        with self.assertRaisesRegex(ValueError, "distinct"):
            contract.validate_config(shared)
        private = json.loads(json.dumps(self.config))
        private["transport"]["fallback_ips"] = ["10.0.0.1"]
        with self.assertRaisesRegex(ValueError, "public IPv4"):
            contract.validate_config(private)

    def test_markdown_becomes_escaped_telegram_html(self):
        rendered = render.render_chunks("# Plan\n**bold** snake_case `a<b>&` [docs](https://example.com/?a=1&b=2) [x](javascript:alert(1))\n> quoted **line**\n- item\n```py\nif a < b:\n    pass\n```", "markdown", 4096)
        self.assertEqual(rendered, ['<b>Plan</b>\n<b>bold</b> snake_case <code>a&lt;b&gt;&amp;</code> <a href="https://example.com/?a=1&amp;b=2">docs</a> [x](javascript:alert(1))\n<blockquote>quoted <b>line</b></blockquote>\n• item\n<pre><code class="language-py">if a &lt; b:\n    pass</code></pre>'])
        self.assertEqual(render.render_chunks("**a `code` b**", "markdown", 4096), ["<b>a </b><code>code</code><b> b</b>"])
        self.assertEqual(render.render_chunks("1 < 2 & 3", "text", 4096), ["1 &lt; 2 &amp; 3"])
        with self.assertRaisesRegex(ValueError, "does not support <p>"):
            render.render_chunks("<p>x</p>", "html", 4096)
        with self.assertRaisesRegex(ValueError, "leaves <b> open"):
            render.render_chunks("<b>x", "html", 4096)
        with self.assertRaisesRegex(ValueError, "empty"):
            render.render_chunks(" \n ", "markdown", 4096)

    def test_long_text_splits_into_balanced_utf16_bounded_messages(self):
        text = "**" + ("bold words " * 600).strip() + "**\n\n" + "😀" * 3000 + "\n\n```\n" + "\n".join(f"line {index} <x>" for index in range(600)) + "\n```"
        chunks = render.render_chunks(text, "markdown", 4096)
        self.assertGreaterEqual(len(chunks), 5)
        for chunk in chunks:
            self.assertLessEqual(render.utf16_len(visible(chunk)), 4096)
            self.assertEqual(render.render_chunks(chunk, "html", 4096), [chunk])
        self.assertTrue(chunks[0].startswith("<b>") and chunks[1].startswith("<b>"))
        self.assertTrue(all(chunk.startswith("<pre>") and chunk.endswith("</pre>") for chunk in chunks[-2:]))
        self.assertEqual("".join(visible(chunk) for chunk in chunks).count("😀"), 3000)

    def test_group_gating_mentions_replies_commands_and_topics(self):
        group = {"id": -100123, "type": "supergroup", "is_forum": True}
        address = lambda value: contract.message_address(self.config, self.access, BOT, value)
        self.assertEqual(address(message("hello")), ("private_chat", None))
        self.assertEqual(address(message("hello", group)), (None, "not_addressed_to_bot"))
        self.assertEqual(address(message("@Octo_Bot status", group, entities=[{"type": "mention", "offset": 0, "length": 9}])), ("mention", None))
        self.assertEqual(address(message("/help@other_bot", group, entities=[{"type": "bot_command", "offset": 0, "length": 15}])), (None, "command_for_another_bot"))
        self.assertEqual(address(message("/help@octo_bot now", group, entities=[{"type": "bot_command", "offset": 0, "length": 14}])), ("command", None))
        reply = message("more", group, reply_to_message={"message_id": 40, "from": {"id": 999, "is_bot": True}, "text": "earlier"})
        self.assertEqual(address(reply), ("reply", None))
        root = message("in topic", group, message_thread_id=40, is_topic_message=True, reply_to_message={"message_id": 40, "from": {"id": 999, "is_bot": True}, "forum_topic_created": {"name": "Ops"}})
        self.assertEqual(address(root), (None, "not_addressed_to_bot"))
        self.assertEqual(contract.topic_of(root), 40)
        self.access["free_chats"].add(-100123)
        self.assertEqual(address(message("hello", group)), ("free_response_chat", None))

    def test_message_content_expands_hidden_links_and_quotes(self):
        text = "😀 see docs"
        content = contract.message_content(message(text, entities=[{"type": "text_link", "offset": 7, "length": 4, "url": "https://docs.example"}], quote={"text": "docs"}, forward_origin={"type": "hidden_user", "sender_user_name": "Bob"}), BOT, [], self.config)
        self.assertEqual(content, {"text": "😀 see docs (https://docs.example)", "quote": "docs", "forwarded_from": "Bob"})
        command = contract.message_content(message("/start K7MQ2", entities=[{"type": "bot_command", "offset": 0, "length": 6}]), BOT, [], self.config)
        self.assertEqual(command["command"], {"name": "start", "args": "K7MQ2"})

    def test_operation_vocabulary_fails_closed(self):
        self.assertEqual(contract.normalize_operation({"text": "hi", "chat_id": "-100123"}, self.config), ("message.send", {"text": "hi", "chat_id": -100123}))
        operation, args = contract.normalize_operation({"operation": "message.send", "args": {"text": "pick", "buttons": [[{"text": "Yes", "data": "yes"}, {"text": "Docs", "url": "https://example.com"}]]}}, self.config)
        self.assertEqual(args["buttons"], [[{"text": "Yes", "callback_data": "yes"}, {"text": "Docs", "url": "https://example.com"}]])
        for body, pattern in (
            ({"operation": "chat.ban", "args": {}}, "not supported"),
            ({"operation": "message.send", "args": {"text": "x", "buttons": [{"text": "flat", "data": "x"}]}}, "rows"),
            ({"operation": "message.send", "args": {"text": "x", "buttons": [[{"text": "big", "data": "x" * 65}]]}}, "64-byte"),
            ({"operation": "file.send", "args": {"blob_id": "a" * 64, "file_id": "f"}}, "exactly one"),
            ({"operation": "api.call", "args": {"method": "banChatMember"}}, "not allowed"),
            ({"operation": "message.edit", "args": {"message_id": 3}}, "text or buttons"),
        ):
            with self.assertRaisesRegex(ValueError, pattern):
                contract.normalize_operation(body, self.config)


class WireTests(unittest.TestCase):
    def setUp(self):
        self.api = BotApi(base_config(), "123456:" + "A" * 35, lambda value: value.replace("secret-description", "[redacted]"))

    def test_proxy_rules_are_explicit(self):
        with self.assertRaisesRegex(ValueError, "must use HTTPS"):
            parse_proxy("http://proxy.example:8080")
        with self.assertRaisesRegex(ValueError, "loopback"):
            parse_proxy("socks5://proxy.example:1080")
        self.assertEqual(parse_proxy("socks5h://user:pass@127.0.0.1:1080")["username"], "user")
        self.assertIsNone(parse_proxy(""))

    def test_rate_limits_wait_and_errors_are_classified(self):
        answers = [
            (429, {}, json.dumps({"ok": False, "error_code": 429, "description": "Too Many Requests", "parameters": {"retry_after": 0}}).encode()),
            (200, {}, json.dumps({"ok": True, "result": {"message_id": 7}}).encode()),
        ]
        with mock.patch.object(self.api, "_exchange", side_effect=answers), mock.patch("telegram_wire.time.sleep") as sleep:
            self.assertEqual(self.api.call("sendMessage", {"chat_id": 1, "text": "x"}), {"message_id": 7})
        sleep.assert_called_once_with(0.0)
        for status, code, description, category in ((409, 409, "Conflict: terminated by other getUpdates request", "conflict"), (401, 401, "Unauthorized", "unauthorized"), (400, 400, "Bad Request: secret-description", "telegram_error")):
            body = json.dumps({"ok": False, "error_code": code, "description": description}).encode()
            with mock.patch.object(self.api, "_exchange", return_value=(status, {}, body)):
                with self.assertRaises(TelegramWireError) as raised:
                    self.api.call("getUpdates")
            self.assertEqual(raised.exception.category, category)
            self.assertNotIn("secret-description", str(raised.exception))
        with mock.patch.object(self.api, "_targets", return_value=["192.0.2.1"]), mock.patch.object(self.api, "_open", side_effect=OSError("down")):
            with self.assertRaises(TelegramWireError) as raised:
                self.api.call("sendMessage", {"chat_id": 1, "text": "x"})
        self.assertEqual(raised.exception.category, "transport_error")


class FakeApi:
    def __init__(self):
        self.calls = []
        self.files = {}
        self.next_message = 500
        self.failure = None

    def call(self, method, params=None, timeout=None, retry_safe=False):
        self.calls.append((method, params or {}))
        if self.failure is not None and method == "sendMessage":
            raise self.failure
        if method == "sendMessage":
            self.next_message += 1
            return {"message_id": self.next_message, "chat": {"id": params["chat_id"]}}
        if method == "getFile":
            return {"file_id": params["file_id"], "file_path": f"photos/{params['file_id']}.jpg"}
        return True

    def call_multipart(self, method, fields, files):
        self.calls.append((method, fields, files))
        return {"message_id": 900, "chat": {"id": fields["chat_id"]}}

    def download(self, file_path, maximum):
        return self.files[file_path]


class LifeTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        root = Path(self.temp.name)
        (root / "blobs").mkdir()
        self.environment = mock.patch.dict(os.environ, {
            "OCTOPUS_INTERFACE_ID": "world_telegram",
            "OCTOPUS_INTERFACE_DIR": str(ROOT),
            "OCTOPUS_VAR_DIR": str(root / "var"),
            "OCTOPUS_MOVEMENTS_JSONL": str(root / "movements.jsonl"),
            "OCTOPUS_BLOBS_DIR": str(root / "blobs"),
            "OCTOPUS_LEDGER_INDEX_SQLITE": str(root / "ledger-index.sqlite"),
            "TELEGRAM_BOT_TOKEN": "123456:" + "B" * 35,
            "TELEGRAM_ALLOWED_USERS": "111",
            "TELEGRAM_HOME_CHAT": "",
        })
        self.environment.start()
        for name in ("telegram_life", "telegram_boundary"):
            sys.modules.pop(name, None)
        self.boundary = importlib.import_module("telegram_boundary")
        self.module = importlib.import_module("telegram_life")
        self.config = self.module.load_config()
        self.config["intake"]["media_group_quiet_ms"] = 0
        self.api = FakeApi()
        self.life = self.module.Life(self.config, self.module.resolve_access(self.config), BOT, self.api)
        self.handed = []

    def tearDown(self):
        self.environment.stop()
        self.temp.cleanup()

    def hand(self, manifest_path, evidence=None):
        manifest = json.loads(Path(manifest_path).read_text(encoding="utf-8"))
        self.handed.append((manifest["label"], json.loads(Path(manifest["payload_path"]).read_text(encoding="utf-8"))))
        return 40 + len(self.handed)

    def deliver(self, *updates):
        self.assertEqual(self.life.persist_updates(list(updates)), len(updates))
        with mock.patch.object(self.boundary, "hand_artifact", side_effect=self.hand):
            self.life.process_pending()

    def test_admitted_private_message_becomes_one_world_event(self):
        update = {"update_id": 10, "message": message("deploy status?", reply_to_message={"message_id": 4, "from": {"id": 999, "is_bot": True}, "text": "Deployed."})}
        self.deliver(update)
        self.assertEqual(self.life.persist_updates([update]), 0)
        self.assertEqual(len(self.handed), 1)
        label, event = self.handed[0]
        self.assertEqual(label, "telegram.event")
        self.assertFalse({"text", "attachments", "task", "prompt", "source_artifact"} & set(event))
        self.assertEqual((event["event"], event["addressed_by"], event["chat"]["id"], event["message_id"], event["from"]["id"]), ("message", "private_chat", 111, 5, 111))
        self.assertEqual(event["message"]["reply_to"], {"message_id": 4, "from": {"id": 999, "is_bot": True}, "from_this_bot": True, "text": "Deployed."})
        self.assertIn((111, None), self.life.typing)
        self.assertFalse(os.listdir(self.module.PENDING_DIR))

    def test_unallowed_and_unaddressed_messages_are_not_handed(self):
        group = {"id": -100555, "type": "supergroup"}
        self.deliver({"update_id": 11, "message": message("hi", sender=222)}, {"update_id": 12, "message": message("chatter", group)})
        self.assertEqual(self.handed, [])
        self.assertEqual(len(self.life.load_state()["processed"]), 2)

    def test_album_becomes_one_event_with_downloaded_files(self):
        photos = []
        for index in (1, 2):
            self.api.files[f"photos/p{index}.jpg"] = f"jpeg-{index}".encode()
            photo = [{"file_id": f"t{index}", "file_unique_id": f"ut{index}", "width": 90, "height": 90, "file_size": 10}, {"file_id": f"p{index}", "file_unique_id": f"u{index}", "width": 900, "height": 900, "file_size": 6}]
            extra = {"caption": "trip"} if index == 1 else {}
            photos.append({"update_id": 20 + index, "message": message(None, message_id=60 + index, media_group_id="album-1", photo=photo, **extra)})
        for update in photos:
            del update["message"]["text"]
        self.deliver(*photos)
        self.assertEqual(len(self.handed), 1)
        content = self.handed[0][1]["message"]
        self.assertEqual((content["text"], content["album_message_ids"]), ("trip", [61, 62]))
        for index, item in enumerate(content["files"], start=1):
            self.assertEqual((item["kind"], item["file_id"], item["sha256"]), ("photo", f"p{index}", hashlib.sha256(f"jpeg-{index}".encode()).hexdigest()))
            self.assertEqual(Path(item["path"]).read_bytes(), f"jpeg-{index}".encode())

    def test_reply_without_chat_reaches_the_single_allowed_user_in_parts(self):
        operation, args = self.life.prepare_output("telegram.operation", json.dumps({"text": "word " * 1700, "reply_to_message_id": 5, "buttons": [[{"text": "OK", "data": "ok"}]]}).encode())
        result = self.life.execute(operation, args)
        sends = [params for method, params in self.api.calls if method == "sendMessage"]
        self.assertEqual(len(sends), 3)
        self.assertEqual({params["chat_id"] for params in sends}, {111})
        self.assertEqual(["reply_parameters" in params for params in sends], [True, False, False])
        self.assertEqual(["reply_markup" in params for params in sends], [False, False, True])
        self.assertEqual(result, {"operation": "message.send", "chat_id": 111, "message_ids": [501, 502, 503]})
        self.assertIn("111:503", self.life.load_state()["bot_messages"])
        with mock.patch.dict(os.environ, {"TELEGRAM_ALLOWED_USERS": "111,222"}):
            access = self.module.resolve_access(self.config)
        with self.assertRaisesRegex(ValueError, "names no chat_id"):
            self.module.Life(self.config, access, BOT, self.api).target({})

    def test_outbound_failures_are_classified_by_certainty(self):
        payload = json.dumps({"text": "hello"}).encode()
        failures = []
        with mock.patch.object(self.boundary, "materialize_movement_payload", return_value=("telegram.operation", payload, "c" * 64)), mock.patch.object(self.boundary, "hand_json", side_effect=lambda label, value, key: failures.append((label, value)) or 77):
            self.api.failure = TelegramWireError("transport_error", "unreachable")
            with self.assertRaises(self.module.RetryOutbound):
                self.life.process_outbound(1, b"x" * 32, "run_a", self.module.now_ms())
            self.assertIsNone(self.boundary.load_effect(1))
            self.api.failure = TelegramWireError("transport_uncertain", "reset after send")
            self.life.process_outbound(2, b"x" * 32, "run_a", self.module.now_ms())
            self.api.failure = TelegramWireError("telegram_error", "chat not found", description="Bad Request: chat not found")
            self.life.process_outbound(3, b"x" * 32, "run_a", self.module.now_ms())
            self.api.failure = None
            self.life.process_outbound(4, b"x" * 32, "run_a", self.module.now_ms())
        self.assertEqual([value.get("failure_class") or value["result"]["operation"] for _, value in failures], ["indeterminate_external_effect", "telegram_refused", "message.send"])
        self.assertEqual([label for label, _ in failures], ["world_telegram.failure", "world_telegram.failure", "telegram.delivery.result"])
        self.assertEqual([self.boundary.load_effect(seq)["status"] for seq in (2, 3, 4)], ["failure_evidenced", "failure_evidenced", "evidenced"])
        with self.assertRaisesRegex(ValueError, "accepts only 'telegram.operation'"):
            self.life.prepare_output("interface.artifact.output", payload)

    def test_tokens_access_and_secrets_fail_closed(self):
        with mock.patch.dict(os.environ, {"TELEGRAM_BOT_TOKEN": ""}):
            with self.assertRaisesRegex(ValueError, "not configured"):
                self.module.resolve_token(self.config)
        with mock.patch.dict(os.environ, {"TELEGRAM_BOT_TOKEN": "not-a-token"}):
            with self.assertRaisesRegex(ValueError, "malformed"):
                self.module.resolve_token(self.config)
        with mock.patch.dict(os.environ, {"TELEGRAM_ALLOWED_USERS": ""}):
            with self.assertRaisesRegex(ValueError, "allowed users are not configured"):
                self.module.resolve_access(self.config)
        sanitize = self.module.make_sanitizer(self.config)
        leaked = sanitize({"url": "https://api.telegram.org/bot123456:" + "B" * 35 + "/getMe", "other": "987654:" + "Z" * 40})
        self.assertNotIn("B" * 35, json.dumps(leaked))
        self.assertNotIn("Z" * 40, json.dumps(leaked))

    def test_ledger_fallback_reads_only_this_interface_outbound(self):
        rows = [
            {"seq": 5, "kind": "InterfaceOutbound", "from": {"Interface": "world_provider_x"}, "to": {"Interface": "world_slack"}},
            {"seq": 6, "kind": "InterfaceArrival", "from": {"Interface": "world_telegram"}, "to": {"Interface": "world_provider_x"}},
            {"seq": 7, "kind": "InterfaceOutbound", "from": {"Interface": "world_provider_x"}, "to": {"Interface": "world_telegram"}, "artifact": {"content": [0] * 32},
             "invocation_metadata": {"runtime": {"run": "run_t", "ee": "ee_t", "worker": "system_worker_operator_task", "checkpoint": "cp_t", "interface": "world_telegram", "epoch": 0}}},
        ]
        Path(os.environ["OCTOPUS_MOVEMENTS_JSONL"]).write_text("".join(json.dumps(row) + "\n" for row in rows), encoding="utf-8")
        self.assertEqual(self.boundary.read_outbound(0, self.config), [(7, [0] * 32, "run_t")])
        self.assertEqual(self.boundary.read_outbound(8, self.config), [])
        Path(os.environ["OCTOPUS_MOVEMENTS_JSONL"]).write_text('{"seq":9}', encoding="utf-8")
        self.boundary._LEDGER_FALLBACK.update({"offset": 0, "size": 0, "identity": None})
        with self.assertRaisesRegex(RuntimeError, "partial"):
            self.boundary.read_ledger(0, 16, 1024)


if __name__ == "__main__":
    unittest.main()
