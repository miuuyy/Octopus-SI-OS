import base64
import datetime
import email
import importlib
import json
import os
import re
import socket
import ssl
import sys
import tempfile
import threading
import unittest
from email import policy
from pathlib import Path
from unittest import mock


ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))

import email_contract as contract
import octopus_operation_catalog as catalog
import email_wire
from octopus_content_hash import octopus_content_hash


PEM = dict(re.findall(r"const TEST_(\w+) = `([^`]+)`", (ROOT / "tests" / "tls_fixture.js").read_text(encoding="utf-8")))
PASSWORD = "test-app-password"


def load_config():
    config = json.loads((ROOT / "config.json").read_text(encoding="utf-8"))
    config["environment"] = json.loads((ROOT / "interface.json").read_text(encoding="utf-8"))["interface_driver_config"]["private_config"]["environment"]
    return contract.validate_config(config)


def message(sender, subject, body, message_id, extra=""):
    return (
        f"From: {sender}\r\nTo: owner@example.test\r\nSubject: {subject}\r\nMessage-ID: {message_id}\r\n"
        f"Date: Thu, 29 Oct 2026 10:00:00 +0000\r\n{extra}MIME-Version: 1.0\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n{body}\r\n"
    ).encode("utf-8")


class FakeServer:
    def __init__(self, tls):
        self.tls = tls
        self.lock = threading.Lock()
        self.listener = socket.socket()
        self.listener.bind(("127.0.0.1", 0))
        self.listener.listen(8)
        self.port = self.listener.getsockname()[1]
        threading.Thread(target=self.accept, daemon=True).start()

    def accept(self):
        while True:
            try:
                raw, _ = self.listener.accept()
            except OSError:
                return
            threading.Thread(target=self.serve, args=(raw,), daemon=True).start()

    def serve(self, raw):
        try:
            with self.tls.wrap_socket(raw, server_side=True) as connection:
                self.session(connection.sendall, connection.makefile("rb"))
        except (OSError, ssl.SSLError):
            pass


class FakeImap(FakeServer):
    def __init__(self, tls):
        self.uidvalidity = 7
        self.boxes = {"INBOX": [], "Sent": []}
        self.next_uid = {"INBOX": 1, "Sent": 1}
        super().__init__(tls)

    def add(self, raw, box="INBOX"):
        with self.lock:
            uid = self.next_uid[box]
            self.next_uid[box] += 1
            self.boxes[box].append({"uid": uid, "raw": raw, "flags": []})
            return uid

    def renumber(self, uidvalidity, offset):
        with self.lock:
            self.uidvalidity = uidvalidity
            for row in self.boxes["INBOX"]:
                row["uid"] += offset
            self.next_uid["INBOX"] += offset

    def session(self, send, reader):
        send(b"* OK [CAPABILITY IMAP4rev1 AUTH=PLAIN UIDPLUS] ready\r\n")
        selected = "INBOX"
        while True:
            line = reader.readline()
            if not line:
                return
            tag, _, rest = line.decode("utf-8").rstrip("\r\n").partition(" ")
            command, _, args = rest.partition(" ")
            command = command.upper()
            if command == "CAPABILITY":
                send(b"* CAPABILITY IMAP4rev1 AUTH=PLAIN UIDPLUS\r\n")
            elif command == "AUTHENTICATE":
                send(b"+ \r\n")
                if base64.b64decode(reader.readline().strip()).split(b"\0")[2].decode("utf-8") != PASSWORD:
                    send(f"{tag} NO [AUTHENTICATIONFAILED] Invalid credentials\r\n".encode())
                    continue
            elif command in ("SELECT", "EXAMINE"):
                selected = args.strip('"')
                with self.lock:
                    rows = self.boxes[selected]
                    send(f"* {len(rows)} EXISTS\r\n* OK [UIDVALIDITY {self.uidvalidity}] ok\r\n* OK [UIDNEXT {self.next_uid[selected]}] ok\r\n".encode())
            elif command == "LIST":
                send(b'* LIST (\\HasNoChildren) "/" "INBOX"\r\n* LIST (\\HasNoChildren \\Sent) "/" "Sent"\r\n')
            elif command == "UID":
                self.uid_command(send, args, selected)
            elif command == "LOGOUT":
                send(f"* BYE bye\r\n{tag} OK done\r\n".encode())
                return
            elif command != "NOOP":
                send(f"{tag} BAD unsupported\r\n".encode())
                continue
            send(f"{tag} OK done\r\n".encode())

    def uid_command(self, send, args, selected):
        verb, _, rest = args.partition(" ")
        with self.lock:
            rows = list(self.boxes[selected])
        if verb.upper() == "SEARCH":
            found = [row["uid"] for row in rows]
            range_match = re.match(r"UID (\d+):\*", rest)
            header_match = re.match(r'HEADER Message-ID "(.+)"', rest)
            if range_match and rows:
                low, high = sorted((int(range_match.group(1)), rows[-1]["uid"]))
                found = [uid for uid in found if low <= uid <= high]
            elif header_match:
                found = [row["uid"] for row in rows if header_match.group(1).encode() in row["raw"]]
            send(("* SEARCH " + " ".join(str(uid) for uid in found)).rstrip().encode() + b"\r\n")
        elif verb.upper() == "FETCH":
            uid = int(rest.split(" ", 1)[0])
            for index, row in enumerate(rows, 1):
                if row["uid"] != uid:
                    continue
                fields = re.search(r"HEADER\.FIELDS \(([^)]*)\)", rest)
                if fields:
                    wanted = {name.lower() for name in fields.group(1).split()}
                    head, keep = [], False
                    for header_line in row["raw"].split(b"\r\n\r\n", 1)[0].split(b"\r\n"):
                        if not header_line[:1].isspace():
                            keep = header_line.split(b":", 1)[0].decode().lower() in wanted
                        if keep:
                            head.append(header_line)
                    body, section = b"\r\n".join(head) + b"\r\n\r\n", f"BODY[HEADER.FIELDS ({fields.group(1)})]<0>"
                else:
                    body, section = row["raw"], "BODY[]<0>"
                prefix = f'* {index} FETCH (UID {uid} FLAGS ({" ".join(row["flags"])}) INTERNALDATE "29-Oct-2026 10:00:00 +0000" RFC822.SIZE {len(row["raw"])} {section} {{{len(body)}}}\r\n'
                send(prefix.encode() + body + b")\r\n")
        elif verb.upper() == "STORE":
            for row in rows:
                if str(row["uid"]) == rest.split(" ", 1)[0] and "\\Seen" not in row["flags"]:
                    row["flags"].append("\\Seen")


class FakeSmtp(FakeServer):
    def __init__(self, tls, imap):
        self.imap = imap
        self.delivered = []
        self.drop_after_data = False
        self.save_sent = True
        super().__init__(tls)

    def session(self, send, reader):
        send(b"220 fake ESMTP\r\n")
        recipients = []
        while True:
            line = reader.readline()
            if not line:
                return
            text = line.decode("utf-8").rstrip("\r\n")
            verb = text.split(" ", 1)[0].upper()
            if verb == "EHLO":
                send(b"250-fake\r\n250-AUTH PLAIN LOGIN\r\n250 8BITMIME\r\n")
            elif verb == "AUTH":
                secret = base64.b64decode(text.split(" ")[2]).split(b"\0")[2].decode("utf-8")
                send(b"235 2.7.0 Accepted\r\n" if secret == PASSWORD else b"535 5.7.8 Username and Password not accepted\r\n")
            elif verb == "RCPT":
                recipients.append(text.split(":", 1)[1].strip("<>"))
                send(b"250 ok\r\n")
            elif verb == "DATA":
                send(b"354 go\r\n")
                lines = []
                while True:
                    chunk = reader.readline()
                    if chunk in (b".\r\n", b""):
                        break
                    lines.append(chunk[1:] if chunk.startswith(b"..") else chunk)
                data = b"".join(lines)
                self.delivered.append((list(recipients), data))
                if self.save_sent:
                    self.imap.add(data, "Sent")
                if self.drop_after_data:
                    return
                send(b"250 2.0.0 OK queued as fake-1\r\n")
            elif verb == "QUIT":
                send(b"221 bye\r\n")
                return
            else:
                send(b"250 ok\r\n")


class FakeRuntime:
    def __init__(self, life):
        self.life = life
        self.seq = 100
        self.handed = []
        self.admitted = {}
        self.refuse_next = None

    def hand(self, manifest_path, evidence):
        manifest_bytes = Path(manifest_path).read_bytes()
        manifest = json.loads(manifest_bytes)
        artifact_id = evidence[0]["artifact_id"]
        assert artifact_id == f"{manifest['label']}:{octopus_content_hash(manifest_bytes)}"
        if self.refuse_next:
            reason, self.refuse_next = self.refuse_next, None
            raise self.life.DeliveryRefused({"refused": {"reason": reason, "detail": {}}})
        if artifact_id in self.admitted:
            raise self.life.DeliveryRefused({"refused": {"reason": "malformed", "detail": {"message": "arrival evidence already exists in the movement ledger", "artifact_id": artifact_id}}})
        self.seq += 1
        self.admitted[artifact_id] = self.seq
        self.handed.append((manifest["label"], json.loads(Path(manifest["payload_path"]).read_bytes())))
        with open(os.environ["OCTOPUS_MOVEMENTS_JSONL"], "a", encoding="utf-8") as handle:
            handle.write(json.dumps({"seq": self.seq, "kind": "InterfaceArrival", "from": {"Interface": "world_email"}, "artifact": {"id": artifact_id}}) + "\n")
        return self.seq


class EmailContractTests(unittest.TestCase):
    def setUp(self):
        self.config = load_config()
        self.environment = self.config["environment"]

    def account(self, **values):
        names = {"address": "address_env", "password": "password_env", "imap": "imap_host_env", "smtp": "smtp_host_env", "allowed": "allowed_senders_env"}
        return contract.resolve_account(self.config, {self.environment[names[key]]: value for key, value in values.items()})

    def test_presets_overrides_and_allowed_senders(self):
        gmail = self.account(address="Someone@GMAIL.com", password="abcd efgh ijkl mnop")
        self.assertEqual((gmail["address"], gmail["password"], gmail["provider"]), ("Someone@gmail.com", "abcdefghijklmnop", "gmail"))
        self.assertEqual((gmail["imap"], gmail["smtp"]["port"]), ({"host": "imap.gmail.com", "port": 993, "tls": "implicit"}, 465))
        icloud = self.account(address="me@icloud.com", password=PASSWORD)
        self.assertEqual((icloud["smtp"]["tls"], icloud["sent_copy"]), ("starttls", "append"))
        with self.assertRaisesRegex(contract.ContractError, "no built-in mail server settings"):
            self.account(address="me@example.org", password=PASSWORD)
        custom = self.account(address="me@example.org", password=PASSWORD, imap="Mail.Example.org:143", smtp="mail.example.org", allowed="Boss@Example.com; @partner.test")
        self.assertEqual((custom["imap"]["tls"], custom["smtp"], custom["provider"]), ("starttls", {"host": "mail.example.org", "port": 465, "tls": "implicit"}, None))
        self.assertEqual((custom["allowed"]["addresses"], custom["allowed"]["domains"], contract.default_recipient(custom)), (["Boss@example.com"], {"partner.test"}, "Boss@example.com"))
        hosted = self.account(address="me@company.test", password=PASSWORD, imap="imap.gmail.com")
        self.assertEqual((hosted["provider"], hosted["smtp"]["host"]), ("gmail", "smtp.gmail.com"))
        strict = json.loads(json.dumps(self.config))
        strict["intake"]["replay"] = True
        with self.assertRaisesRegex(contract.ContractError, "does not accept replay"):
            contract.validate_config(strict)

    def test_html_body_attachments_and_raw_utf8_headers(self):
        raw = (
            "From: =?utf-8?b?0JDQvdC90LA=?= <anna@example.org>\r\nTo: owner@example.test\r\nSubject: Привет мир\r\nMessage-ID: <m1@example.org>\r\n"
            "MIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=b1\r\n\r\n--b1\r\nContent-Type: multipart/alternative; boundary=b2\r\n\r\n"
            "--b2\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n\r\n--b2\r\nContent-Type: text/html; charset=utf-8\r\n\r\n"
            "<html><head><style>p{}</style></head><body><p>Hello <b>World</b></p><ul><li>one</li><li>two</li></ul>"
            "<blockquote><p>quoted line</p></blockquote><a href=\"https://x.io/a\">link</a> &amp; more<br>next</body></html>\r\n--b2--\r\n"
            "--b1\r\nContent-Type: text/csv; name=data.csv\r\nContent-Disposition: attachment; filename=\"../data.csv\"\r\n\r\na,b\r\n1,2\r\n--b1--\r\n"
        ).encode("utf-8")
        stored = []
        parsed = contract.parse_message(raw, self.config["content"], 1000, lambda data, name, digest: stored.append((name, digest)) or f"/store/{digest}/{name}")
        self.assertEqual((parsed["subject"], parsed["from"]), ("Привет мир", {"address": "anna@example.org", "name": "Анна"}))
        self.assertEqual(parsed["body"]["source"], "text/html")
        self.assertEqual(parsed["body"]["text"], "Hello World\n\n- one\n- two\n\n> quoted line\n\nlink (https://x.io/a) & more\nnext")
        attachment = parsed["attachments"][0]
        self.assertEqual((attachment["filename"], attachment["text_preview"], stored[0][0]), ("data.csv", "a,b\r\n1,2", "data.csv"))
        self.assertEqual(attachment["path"], f"/store/{attachment['sha256']}/data.csv")
        self.assertEqual(parsed["media"], [])
        png = b"\x89PNG\r\n\x1a\n" + b"pixels"
        image_raw = raw.replace(b"text/csv; name=data.csv", b"image/png; name=shot.png").replace(b"../data.csv", b"shot.png").replace(b"a,b\r\n1,2", base64.b64encode(png)).replace(b"\r\n\r\n" + base64.b64encode(png), b"\r\nContent-Transfer-Encoding: base64\r\n\r\n" + base64.b64encode(png))
        parsed = contract.parse_message(image_raw, self.config["content"], 1000, lambda data, name, digest: f"/store/{digest}/{name}")
        event = contract.inbound_event({"address": "owner@example.test"}, {"mailbox": "INBOX", "uidvalidity": 1, "uid": 1}, {"trusted": True, "sender": {}}, parsed, [], None)
        self.assertEqual((event["message"]["attachments"][0]["image"], event["media"][0]["mime_type"], event["media"][0]["artifact"]["label"]), (0, "image/png", "email.file"))
        self.assertEqual((base64.b64decode(event["media"][0]["payload_base64"]), event["media"][0]["artifact"]["payload_path"]), (png, event["message"]["attachments"][0]["path"]))

    def test_reply_threading_recipients_and_quote(self):
        account = self.account(address="owner@gmail.com", password=PASSWORD)
        original = {
            "message_id": "<m3@example.org>", "in_reply_to": ["<m2@example.org>"], "references": ["<m1@example.org>", "<m2@example.org>"],
            "from": {"address": "alice@example.org", "name": "Alice"}, "reply_to": [], "subject": "Re: Plan",
            "to": [{"address": "owner@gmail.com", "name": ""}, {"address": "bob@example.org", "name": "Bob"}],
            "cc": [{"address": "carol@example.org", "name": ""}], "date_header": "Thu, 29 Oct 2026 10:00:00 +0000",
            "body": {"text": "Can you check?\nThanks"},
        }
        operation = "message.reply"
        args = contract.normalize_arguments(operation, {"message_id": "<m3@example.org>", "text": "Checked.", "reply_all": True, "quote": True}, self.config)
        composed, recipients, summary = contract.compose_message(operation, args, account, self.config["outbound"], original, [], "<x1@gmail.com>", datetime.datetime(2026, 10, 30, tzinfo=datetime.timezone.utc))
        parsed = email.message_from_bytes(contract.serialize_message(composed), policy=policy.default)
        self.assertEqual((parsed["Subject"], parsed["In-Reply-To"], parsed["References"]), ("Re: Plan", "<m3@example.org>", "<m1@example.org> <m2@example.org> <m3@example.org>"))
        self.assertEqual((parsed["To"], parsed["Cc"], parsed["Auto-Submitted"]), ("Alice <alice@example.org>", "Bob <bob@example.org>, carol@example.org", "auto-generated"))
        self.assertEqual(recipients, ["alice@example.org", "bob@example.org", "carol@example.org"])
        self.assertIn("Checked.\n\nOn Thu, 29 Oct 2026 10:00:00 +0000, Alice <alice@example.org> wrote:\n> Can you check?\n> Thanks", parsed.get_content().replace("\r\n", "\n"))
        self.assertEqual(summary["in_reply_to"], "<m3@example.org>")
        private = json.loads((ROOT / "interface.json").read_text(encoding="utf-8"))["interface_driver_config"]["private_config"]
        loaded = contract.validate_handlers(catalog.load_catalog(str(ROOT), private, "email"))
        self.assertEqual((loaded["request_label"], loaded["result_label"], len(loaded["operations"])), ("email.operation.request", "email.operation.result", 8))
        with self.assertRaisesRegex(ValueError, "missing subject"):
            catalog.validate(loaded["operations"]["email.send_message"]["input_schema"], {"text": "x"})
        for operation, args, pattern in (
            ("message.reply", {"text": "x"}, "exactly one of ref or message_id"),
            ("message.search", {"since": "29.10.2026"}, "YYYY-MM-DD"),
            ("message.send", {"subject": "s", "text": "x", "to": "a@b.test, c@d.test"}, "exactly one address"),
        ):
            with self.assertRaisesRegex(contract.ContractError, pattern):
                contract.normalize_arguments(operation, args, self.config)

    def test_admission_allowlist_trust_and_forged_results(self):
        account = self.account(address="owner@gmail.com", password=PASSWORD, allowed="alice@example.org")
        verified = "Authentication-Results: mx.google.com; dkim=pass header.i=@example.org; spf=pass smtp.mailfrom=alice@example.org; dmarc=pass header.from=example.org\r\n"
        forged = "Authentication-Results: mx.google.com; dmarc=fail header.from=example.org\r\nAuthentication-Results: mx.google.com; dmarc=pass header.from=example.org\r\n"

        def decide(raw, own=()):
            return contract.admission(contract.parse_header_block(raw.split(b"\r\n\r\n", 1)[0] + b"\r\n\r\n"), account, self.config, set(own))

        allowed = decide(message("Alice <alice@example.org>", "Hi", "x", "<a1@example.org>", verified))
        self.assertEqual((allowed["admit"], allowed["trusted"], allowed["sender"]["allowed_as"], allowed["sender"]["verification"]), (True, True, "allowed_sender", "dmarc=pass"))
        spoofed = decide(message("alice@example.org", "Hi", "x", "<a2@example.org>", forged))
        self.assertEqual((spoofed["admit"], spoofed["trusted"]), (True, False))
        self.assertEqual(decide(message("mallory@evil.test", "Hi", "x", "<a3@evil.test>"))["reason"], "sender is not allowed")
        self.assertEqual(decide(message("alice@example.org", "News", "x", "<a4@example.org>", "List-Unsubscribe: <mailto:u@example.org>\r\n"))["reason"], "List-Unsubscribe header")
        self.assertEqual(decide(message("owner@gmail.com", "Re: Hi", "x", "<own@gmail.com>"), own=["<own@gmail.com>"])["reason"], "sent by this interface")


class EmailLifeTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        root = Path(self.temp.name)
        for name in ("blobs", "var", "hand-in"):
            (root / name).mkdir()
        (root / "leaf.pem").write_text(PEM["LEAF_CERT"], encoding="utf-8")
        (root / "leaf.key").write_text(PEM["LEAF_KEY"], encoding="utf-8")
        server_tls = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        server_tls.load_cert_chain(str(root / "leaf.pem"), str(root / "leaf.key"))
        client_tls = ssl.create_default_context(cadata=PEM["CA_CERT"])
        self.imap = FakeImap(server_tls)
        self.smtp = FakeSmtp(server_tls, self.imap)
        self.patches = [
            mock.patch.dict(os.environ, {
                "OCTOPUS_INTERFACE_ID": "world_email",
                "OCTOPUS_INTERFACE_DIR": str(ROOT),
                "OCTOPUS_VAR_DIR": str(root / "var"),
                "OCTOPUS_MOVEMENTS_JSONL": str(root / "movements.jsonl"),
                "OCTOPUS_BLOBS_DIR": str(root / "blobs"),
                "OCTOPUS_LEDGER_INDEX_SQLITE": str(root / "ledger-index.sqlite"),
                "OCTOPUS_INTERFACE_HOST_IO_LIMITS": json.dumps({"max_control_frame_bytes": 1048576, "max_manifest_bytes": 1048576, "max_payload_bytes": 67108864, "max_stderr_bytes": 262144}),
                "OCTOPUS_ROOT_SLOTS_JSON": json.dumps({"interface_hand_in_root": str(root / "hand-in")}),
                "EMAIL_ADDRESS": "owner@example.test",
                "EMAIL_APP_PASSWORD": PASSWORD,
                "EMAIL_IMAP_SERVER": f"localhost:{self.imap.port}",
                "EMAIL_SMTP_SERVER": f"localhost:{self.smtp.port}",
                "EMAIL_ALLOWED_SENDERS": "alice@example.org, @partner.test",
            }),
            mock.patch.object(email_wire, "tls_context", return_value=client_tls),
        ]
        for patch in self.patches:
            patch.start()
        sys.modules.pop("email_life", None)
        self.life = importlib.import_module("email_life")
        self.life.prepare_directories()
        self.life.CONFIG = self.life.load_config()
        account = contract.resolve_account(self.life.CONFIG, os.environ)
        account["authserv_ids"] = ["mx.example.test"]
        store = self.life.AttachmentStore(self.life.attachment_root())
        store.ensure()
        self.mailbox = self.life.Mailbox(self.life.CONFIG, account, store)
        self.runtime = FakeRuntime(self.life)
        self.patches.append(mock.patch.object(self.life, "hand_artifact", side_effect=self.runtime.hand))
        self.patches[-1].start()

    def tearDown(self):
        for patch in reversed(self.patches):
            patch.stop()
        self.imap.listener.close()
        self.smtp.listener.close()
        self.temp.cleanup()

    def catch_up(self):
        session = self.mailbox.imap()
        try:
            return self.life.Watcher(self.mailbox, "INBOX").catch_up(session, session.select("INBOX", readonly=True))
        finally:
            session.close()

    def last_uid(self):
        return self.life.load_state(self.mailbox.account)["mailboxes"]["INBOX"]["last_uid"]

    def request(self, seq, operation, arguments, label="email.operation.request"):
        payload = json.dumps({"operation": operation, "arguments": arguments, "correlation_id": f"call-{seq}"}).encode("utf-8")
        manifest = json.dumps({"label": label, "payload_content_hash": octopus_content_hash(payload)}).encode("utf-8")
        for blob in (payload, manifest):
            (Path(os.environ["OCTOPUS_BLOBS_DIR"]) / octopus_content_hash(blob)).write_bytes(blob)
        movement = {
            "seq": seq, "kind": "InterfaceOutbound", "from": {"Interface": "world_provider_test"}, "to": {"Interface": "world_email"},
            "invocation_metadata": {"runtime": {"run": "run_mail", "ee": "ee_mail", "worker": "system_worker_operator_task", "checkpoint": "cp_mail", "interface": "world_email", "epoch": 0}},
            "artifact": {"content": list(bytes.fromhex(octopus_content_hash(manifest)))},
        }
        with open(os.environ["OCTOPUS_MOVEMENTS_JSONL"], "a", encoding="utf-8") as handle:
            handle.write(json.dumps(movement) + "\n")
        outbound = self.life.Outbound(self.mailbox)
        for row in self.life.OutboundReader().read(seq):
            outbound.process(*row)
        return self.runtime.handed[-1]

    def test_intake_cursor_never_replays_loses_or_duplicates(self):
        self.imap.add(message("mallory@evil.test", "Old", "old", "<old1@evil.test>"))
        self.imap.add(message("alice@example.org", "Old", "old", "<old2@example.org>"))
        self.catch_up()
        self.assertEqual((self.runtime.handed, self.last_uid()), ([], 2))
        trusted = "Authentication-Results: mx.example.test; dmarc=pass header.from=example.org\r\n"
        self.imap.add(message("Alice <alice@example.org>", "Plan", "Please check the plan.", "<plan@example.org>", trusted))
        self.imap.add(message("bob@partner.test", "Digest", "news", "<digest@partner.test>", "List-Id: <digest.partner.test>\r\n"))
        self.imap.add(message("mallory@evil.test", "Hi", "spam", "<spam@evil.test>"))
        self.catch_up()
        self.assertEqual((len(self.runtime.handed), self.last_uid()), (1, 5))
        label, event = self.runtime.handed[0]
        self.assertEqual((label, event["event"], event["trusted"], event["sender"]["allowed_as"]), ("email.event", "message.received", True, "allowed_sender"))
        self.assertEqual((event["message"]["ref"], event["message"]["subject"], event["message"]["body"]["text"]), ({"mailbox": "INBOX", "uidvalidity": 7, "uid": 3}, "Plan", "Please check the plan."))
        self.catch_up()
        self.assertEqual(len(self.runtime.handed), 1)
        self.imap.add(message("alice@example.org", "Second", "Another one.", "<second@example.org>"))
        self.runtime.refuse_next = "registry_unstable"
        with self.assertRaises(self.life.Paused):
            self.catch_up()
        self.assertEqual(self.last_uid(), 5)
        self.catch_up()
        self.assertEqual((len(self.runtime.handed), self.last_uid()), (2, 6))
        key = self.life.message_key({"message_id": "<second@example.org>"}, "alice@example.org")

        def crash_before_cursor(state):
            state["admitted"].remove(key)
            state["mailboxes"]["INBOX"]["last_uid"] = 5

        self.life.update_state(self.mailbox.account, crash_before_cursor)
        self.catch_up()
        self.assertEqual((len(self.runtime.handed), self.last_uid()), (2, 6))
        self.imap.boxes["INBOX"] = self.imap.boxes["INBOX"][2:]
        self.imap.renumber(8, 100)
        self.catch_up()
        self.assertEqual(len(self.runtime.handed), 2)
        entry = self.life.load_state(self.mailbox.account)["mailboxes"]["INBOX"]
        self.assertEqual((entry["uidvalidity"], entry["last_uid"]), (8, 106))

    def test_reply_threads_and_an_interrupted_send_is_never_repeated(self):
        png = b"\x89PNG\r\n\x1a\n" + b"chart"
        raw = message("Alice <alice@example.org>", "Plan", "Please check.", "<plan@example.org>", "References: <root@example.org>\r\n")
        raw = raw.replace(b"Content-Type: text/plain; charset=utf-8\r\n\r\nPlease check.\r\n", b"Content-Type: multipart/mixed; boundary=b\r\n\r\n--b\r\nContent-Type: text/plain\r\n\r\nPlease check.\r\n--b\r\nContent-Type: text/csv\r\nContent-Disposition: attachment; filename=plan.csv\r\n\r\nstep,owner\r\n--b\r\nContent-Type: image/png\r\nContent-Disposition: attachment; filename=chart.png\r\nContent-Transfer-Encoding: base64\r\n\r\n" + base64.b64encode(png) + b"\r\n--b--\r\n")
        self.imap.add(raw)
        self.life.update_state(self.mailbox.account, lambda state: state["mailboxes"].update(INBOX={"uidvalidity": 7, "last_uid": 0, "checked_at": self.life.now_iso()}))
        self.catch_up()
        event = self.runtime.handed[0][1]["message"]
        stored = event["attachments"][0]["path"]
        self.assertTrue(stored.startswith(self.life.attachment_root()))
        label, read = self.request(1, "email.read_message", {"ref": event["ref"]})
        self.assertEqual((label, read["operation"], read["correlation_id"], read["ok"]), ("email.operation.result", "email.read_message", "call-1", True))
        self.assertEqual((read["result"]["message"]["attachments"][1]["image"], read["media"][0]["mime_type"], base64.b64decode(read["media"][0]["payload_base64"])), (0, "image/png", png))
        label, receipt = self.request(2, "email.reply", {"ref": event["ref"], "text": "Looks good.", "quote": True, "attachments": [{"path": stored}]})
        self.assertEqual((label, receipt["ok"], receipt["correlation_id"], receipt["result"]["accepted"]), ("email.operation.result", True, "call-2", ["alice@example.org"]))
        recipients, data = self.smtp.delivered[0]
        sent = email.message_from_bytes(data, policy=policy.default)
        self.assertEqual((recipients, sent["Subject"], sent["In-Reply-To"], sent["References"]), (["alice@example.org"], "Re: Plan", "<plan@example.org>", "<root@example.org> <plan@example.org>"))
        self.assertEqual(sent["Message-ID"], receipt["result"]["message_id"])
        self.assertEqual([part.get_filename() for part in sent.iter_attachments()], ["plan.csv"])
        self.assertIn("Looks good.\n\nOn Thu, 29 Oct 2026 10:00:00 +0000, Alice <alice@example.org> wrote:\n> Please check.", sent.get_body(("plain",)).get_content().replace("\r\n", "\n"))
        outbound = self.life.Outbound(self.mailbox)
        for row in self.life.OutboundReader().read(2):
            outbound.process(*row)
        self.assertEqual(len(self.smtp.delivered), 1)
        self.smtp.drop_after_data = True
        label, receipt = self.request(3, "email.send_message", {"subject": "Report", "text": "Done."})
        self.assertEqual((label, receipt["result"]["confirmed_in"], receipt["result"]["recovered"], receipt["result"]["to"]), ("email.operation.result", "Sent", True, ["alice@example.org"]))
        self.smtp.save_sent = False
        with mock.patch.object(self.life.time, "sleep"):
            label, failure = self.request(4, "email.send_message", {"to": "bob@partner.test", "subject": "Again", "text": "Once."})
        self.assertEqual((label, failure["ok"], failure["error"]["code"]), ("email.operation.result", False, "indeterminate_external_effect"))
        outbound = self.life.Outbound(self.mailbox)
        for row in self.life.OutboundReader().read(4):
            outbound.process(*row)
        self.assertEqual(len(self.smtp.delivered), 3)
        own = email.message_from_bytes(self.smtp.delivered[2][1], policy=policy.default)["Message-ID"]
        self.assertIn(own, self.life.own_message_ids())
        for seq, operation, arguments, code in ((5, "email.erase", {}, "unknown_operation"), (6, "email.mark_messages", {"ref": event["ref"]}, "invalid_arguments")):
            self.assertEqual(self.request(seq, operation, arguments)[1]["error"]["code"], code)
        self.assertEqual(os.listdir(self.life.REQUEST_DIR), [])


if __name__ == "__main__":
    unittest.main()
