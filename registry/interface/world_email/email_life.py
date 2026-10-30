#!/usr/bin/env python3
import base64
import datetime
import hashlib
import json
import mimetypes
import os
import re
import sqlite3
import stat
import sys
import threading
import time
import urllib.parse

from octopus_content_hash import octopus_content_hash

import email_contract as contract
from email_contract import ContractError
from email_wire import CommitUncertain
from email_wire import ImapSession
from email_wire import MailError
from email_wire import SmtpSession
from octopus_operation_catalog import RequestError
from octopus_operation_catalog import answer
from octopus_operation_catalog import load_catalog
from octopus_operation_catalog import parse_request


INTERFACE_ID = os.environ["OCTOPUS_INTERFACE_ID"]
INTERFACE_DIR = os.environ["OCTOPUS_INTERFACE_DIR"]
VAR_DIR = os.path.abspath(os.environ["OCTOPUS_VAR_DIR"])
MOVEMENTS_JSONL = os.environ["OCTOPUS_MOVEMENTS_JSONL"]
BLOBS_DIR = os.environ["OCTOPUS_BLOBS_DIR"]
LEDGER_INDEX_SQLITE = os.environ["OCTOPUS_LEDGER_INDEX_SQLITE"]
CONFIG_PATH = os.path.join(INTERFACE_DIR, "config.json")
DESCRIPTOR_PATH = os.path.join(INTERFACE_DIR, "interface.json")
CURSOR_PATH = os.path.join(VAR_DIR, "ledger-cursor.json")
STATE_PATH = os.path.join(VAR_DIR, "mailbox-state.json")
SENT_IDS_PATH = os.path.join(VAR_DIR, "sent-message-ids.json")
PAYLOAD_DIR = os.path.join(VAR_DIR, "payloads")
REQUEST_DIR = os.path.join(VAR_DIR, "operation-requests")
LEDGER_INDEX_SCHEMA = "4"
SENT_ID_RETENTION = 5000
DOCUMENT_MAX_BYTES = 16 * 1024 * 1024
FAILURE_CLASSES = {
    "authentication": "authentication_failed",
    "transport": "connection_failed",
    "tls": "connection_failed",
    "refused": "refused_by_server",
    "not_found": "not_found",
    "protocol": "server_error",
}

_STDOUT_LOCK = threading.Lock()
_VERDICT_CONDITION = threading.Condition()
_VERDICTS = {}
_VERDICT_INPUT_ERROR = None
_NEXT_REQUEST_ID = 1
_STATE_LOCK = threading.RLock()
_SECRETS = []


class DeliveryRefused(RuntimeError):
    def __init__(self, verdict):
        self.verdict = verdict if isinstance(verdict, dict) else {}
        refused = self.verdict.get("refused") if isinstance(self.verdict.get("refused"), dict) else {}
        self.reason = refused.get("reason")
        self.detail = refused.get("detail") if isinstance(refused.get("detail"), dict) else {}
        super().__init__(f"runtime refused the email artifact: {self.reason or verdict!r}")

    def transient(self):
        return self.reason == "registry_unstable" or (self.reason == "stale_epoch" and self.detail.get("reason_detail") == "arrival_target_changed")

    def duplicate_of(self, artifact_id):
        return self.detail.get("artifact_id") == artifact_id and self.detail.get("message") == "arrival evidence already exists in the movement ledger"


class Paused(RuntimeError):
    pass


def log(message):
    sys.stderr.write(sanitize(f"email {INTERFACE_ID}: {message}") + "\n")
    sys.stderr.flush()


def register_secret(account):
    _SECRETS.clear()
    password = account["password"]
    plain = b"\0" + account["address"].encode("utf-8") + b"\0" + password.encode("utf-8")
    _SECRETS.extend(sorted({password, base64.b64encode(plain).decode("ascii"), base64.b64encode(password.encode("utf-8")).decode("ascii")}, key=len, reverse=True))


def sanitize(value):
    if isinstance(value, dict):
        return {sanitize(key): sanitize(item) for key, item in value.items()}
    if isinstance(value, list):
        return [sanitize(item) for item in value]
    if isinstance(value, str):
        for secret in _SECRETS:
            if len(secret) >= 4:
                value = value.replace(secret, "[secret]")
        return value
    return value


def ensure_private_directory(path):
    os.makedirs(path, mode=0o700, exist_ok=True)
    current = os.lstat(path)
    if stat.S_ISLNK(current.st_mode) or not stat.S_ISDIR(current.st_mode):
        raise PermissionError(f"email private directory is not a real directory: {path}")
    if current.st_uid != os.getuid():
        raise PermissionError(f"email private directory has a different owner: {path}")
    if stat.S_IMODE(current.st_mode) != 0o700:
        os.chmod(path, 0o700)
    return path


def prepare_directories():
    for directory in (VAR_DIR, PAYLOAD_DIR, REQUEST_DIR):
        ensure_private_directory(directory)


def read_bounded(path, maximum, name):
    descriptor = os.open(path, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0))
    try:
        opened = os.fstat(descriptor)
        linked = os.lstat(path)
        if not stat.S_ISREG(opened.st_mode) or stat.S_ISLNK(linked.st_mode) or (opened.st_dev, opened.st_ino) != (linked.st_dev, linked.st_ino):
            raise ValueError(f"email {name} is not a stable regular file")
        if opened.st_size > maximum:
            raise ValueError(f"email {name} exceeds {maximum} bytes")
        with os.fdopen(descriptor, "rb") as handle:
            descriptor = -1
            value = handle.read(maximum + 1)
        if len(value) > maximum:
            raise ValueError(f"email {name} grew above {maximum} bytes")
        return value
    finally:
        if descriptor >= 0:
            os.close(descriptor)


def atomic_bytes(path, payload):
    parent = os.path.dirname(path)
    temporary = os.path.join(parent, f".{os.path.basename(path)}.{os.getpid()}.{threading.get_ident()}.{time.time_ns()}.tmp")
    descriptor = os.open(temporary, os.O_CREAT | os.O_EXCL | os.O_WRONLY | getattr(os, "O_NOFOLLOW", 0), 0o600)
    try:
        with os.fdopen(descriptor, "wb") as handle:
            descriptor = -1
            handle.write(payload)
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(temporary, path)
        directory = os.open(parent, os.O_RDONLY)
        try:
            os.fsync(directory)
        finally:
            os.close(directory)
    finally:
        if descriptor >= 0:
            os.close(descriptor)
        try:
            os.unlink(temporary)
        except FileNotFoundError:
            pass


def canonical_json(value):
    return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":"), allow_nan=False).encode("utf-8")


def atomic_json(path, value):
    atomic_bytes(path, canonical_json(value))


def read_json(path, name):
    try:
        return json.loads(read_bounded(path, DOCUMENT_MAX_BYTES, name))
    except FileNotFoundError:
        return None


def io_limits():
    value = contract.require_keys(json.loads(os.environ["OCTOPUS_INTERFACE_HOST_IO_LIMITS"]), {"max_control_frame_bytes", "max_manifest_bytes", "max_payload_bytes", "max_stderr_bytes"}, set(), "email host I/O limits")
    return {key: contract.require_int(value[key], f"email host I/O limits.{key}", 1) for key in value}


def read_private_config():
    descriptor = contract.require_object(json.loads(read_bounded(DESCRIPTOR_PATH, DOCUMENT_MAX_BYTES, "interface descriptor")), "email interface descriptor")
    if descriptor.get("id") != INTERFACE_ID:
        raise ContractError("email interface descriptor identity does not match its folder")
    return contract.require_object(contract.require_object(descriptor.get("interface_driver_config"), "email interface_driver_config").get("private_config"), "email private_config")


def load_config():
    config = json.loads(read_bounded(CONFIG_PATH, DOCUMENT_MAX_BYTES, "interface config"))
    contract.require_object(config, "email config")["environment"] = contract.validate_environment(read_private_config().get("environment"))
    return contract.validate_config(config)


def load_operations():
    return contract.validate_handlers(load_catalog(INTERFACE_DIR, read_private_config(), "email"))


def attachment_root():
    try:
        slots = json.loads(os.environ["OCTOPUS_ROOT_SLOTS_JSON"])
        root = slots["interface_hand_in_root"]
    except (KeyError, TypeError, ValueError) as error:
        raise ContractError("email needs the interface_hand_in_root slot to store attachments") from error
    if not isinstance(root, str) or not os.path.isabs(root):
        raise ContractError("email interface_hand_in_root is not an absolute path")
    return os.path.join(root, INTERFACE_ID, "attachments")


def _runtime_verdict_reader():
    global _VERDICT_INPUT_ERROR
    try:
        for line in sys.stdin:
            message = json.loads(line)
            if not isinstance(message, dict) or message.get("kind") != "delivery_verdict":
                raise RuntimeError(f"unexpected runtime input {message!r}")
            request_id = message.get("request_id")
            if isinstance(request_id, bool) or not isinstance(request_id, int):
                raise RuntimeError("runtime verdict has no integer request_id")
            with _VERDICT_CONDITION:
                _VERDICTS[request_id] = message.get("verdict") or {}
                _VERDICT_CONDITION.notify_all()
        raise RuntimeError("runtime verdict input closed")
    except Exception as error:
        with _VERDICT_CONDITION:
            _VERDICT_INPUT_ERROR = error
            _VERDICT_CONDITION.notify_all()


def write_frame(value):
    with _STDOUT_LOCK:
        sys.stdout.write(json.dumps(value, separators=(",", ":")) + "\n")
        sys.stdout.flush()


def hand_artifact(manifest_path, evidence):
    global _NEXT_REQUEST_ID
    with _VERDICT_CONDITION:
        request_id = _NEXT_REQUEST_ID
        _NEXT_REQUEST_ID += 1
    write_frame({"kind": "arrival", "request_id": request_id, "artifact_manifest_path": manifest_path, "evidence": evidence})
    with _VERDICT_CONDITION:
        while request_id not in _VERDICTS and _VERDICT_INPUT_ERROR is None:
            _VERDICT_CONDITION.wait()
        if request_id not in _VERDICTS:
            raise RuntimeError(f"runtime verdict input failed before email request {request_id}: {_VERDICT_INPUT_ERROR}")
        verdict = _VERDICTS.pop(request_id)
    admitted = verdict.get("admitted") if isinstance(verdict, dict) else None
    if isinstance(admitted, dict) and isinstance(admitted.get("seq"), int) and not isinstance(admitted.get("seq"), bool):
        return admitted["seq"]
    raise DeliveryRefused(verdict)


def payload_paths(stem):
    if not re.fullmatch(r"[0-9A-Za-z._-]{1,160}", stem):
        raise ValueError("email artifact stem contains unsafe characters")
    return os.path.join(PAYLOAD_DIR, f"{stem}.payload"), os.path.join(PAYLOAD_DIR, f"{stem}.artifact.json")


def materialize(stem, label, payload=None):
    payload_path, manifest_path = payload_paths(stem)
    try:
        payload = read_bounded(payload_path, io_limits()["max_payload_bytes"], "artifact payload")
    except FileNotFoundError:
        if payload is None:
            raise
        if len(payload) > io_limits()["max_payload_bytes"]:
            raise ValueError("email artifact payload exceeds the host payload limit")
        atomic_bytes(payload_path, payload)
    manifest = canonical_json({"label": label, "payload_path": payload_path, "payload_content_hash": octopus_content_hash(payload)})
    try:
        current = read_bounded(manifest_path, io_limits()["max_manifest_bytes"], "artifact manifest")
    except FileNotFoundError:
        current = None
    if current != manifest:
        atomic_bytes(manifest_path, manifest)
    return manifest_path, f"{label}:{octopus_content_hash(manifest)}"


def materialized(stem):
    return os.path.exists(payload_paths(stem)[0])


def hand_json(label, value, stem):
    media = value.get("media") if isinstance(value, dict) else None
    clean = sanitize({key: item for key, item in value.items() if key != "media"} if media is not None else value)
    if media is not None:
        clean["media"] = media
    payload = None if value is None else canonical_json(clean)
    manifest_path, artifact_id = materialize(stem, label, payload)
    digest = bytes.fromhex(octopus_content_hash(artifact_id.encode("utf-8")))
    evidence = [{"interface_id": INTERFACE_ID, "artifact_id": artifact_id, "label": "email.arrival.idempotency", "bytes": list(digest)}]
    try:
        return hand_artifact(manifest_path, evidence)
    except DeliveryRefused as refusal:
        if refusal.duplicate_of(artifact_id):
            admitted = find_admitted_arrival_seq(artifact_id)
            if admitted is not None:
                return admitted
        raise


def open_index():
    uri = f"file:{urllib.parse.quote(os.path.abspath(LEDGER_INDEX_SQLITE))}?mode=ro"
    connection = sqlite3.connect(uri, uri=True, timeout=0.25)
    try:
        if connection.execute("SELECT value FROM index_meta WHERE key = 'schema_version'").fetchone() != (LEDGER_INDEX_SCHEMA,):
            raise RuntimeError("the ledger index has an unsupported schema")
        watermark = connection.execute("SELECT value FROM index_meta WHERE key = 'watermark'").fetchone()
        if watermark is None or not str(watermark[0]).isdigit():
            raise RuntimeError("the ledger index has no watermark")
    except Exception:
        connection.close()
        raise
    return connection, int(watermark[0])


def find_admitted_arrival_seq(artifact_id):
    try:
        connection, _ = open_index()
        try:
            row = connection.execute(
                "SELECT seq FROM movements WHERE kind = 'InterfaceArrival' AND from_kind = 'interface' AND from_id = ? AND artifact_id = ? ORDER BY seq ASC LIMIT 1",
                (INTERFACE_ID, artifact_id),
            ).fetchone()
        finally:
            connection.close()
        if row is not None:
            return int(row[0])
    except (OSError, RuntimeError, ValueError, sqlite3.DatabaseError):
        pass
    for movement, _, _ in ledger_rows(0):
        if movement.get("kind") == "InterfaceArrival" and movement.get("from") == {"Interface": INTERFACE_ID}:
            artifact = movement.get("artifact")
            if isinstance(artifact, dict) and artifact.get("id") == artifact_id:
                return contract.require_int(movement.get("seq"), "email admitted arrival seq", 0)
    return None


def ledger_rows(offset):
    maximum = CONFIG["ledger"]["max_row_bytes"]
    try:
        handle = open(MOVEMENTS_JSONL, "rb")
    except FileNotFoundError:
        return
    with handle:
        handle.seek(offset)
        while True:
            start = handle.tell()
            line = handle.readline(maximum + 2)
            if len(line) > maximum + 1:
                raise RuntimeError(f"the ledger row at byte {start} exceeds {maximum} bytes")
            if not line or not line.endswith(b"\n"):
                return
            yield contract.require_object(json.loads(line), "email ledger movement"), start, handle.tell()


def outbound_runtime(movement):
    metadata = contract.require_object(movement.get("invocation_metadata"), "email outbound invocation_metadata")
    runtime = contract.require_object(metadata.get("runtime"), "email outbound runtime metadata")
    for field in ("run", "ee", "worker", "checkpoint", "interface"):
        contract.require_string(runtime.get(field), f"email outbound runtime.{field}")
    contract.require_int(runtime.get("epoch"), "email outbound runtime.epoch", 0)
    if runtime["interface"] != INTERFACE_ID:
        raise ValueError("email outbound runtime interface does not match its exact target")
    return runtime


def interface_endpoint(value):
    if isinstance(value, dict) and set(value) == {"Interface"} and isinstance(value["Interface"], str):
        return value["Interface"]
    return None


def own_outbound(movement):
    if movement.get("kind") != "InterfaceOutbound" or interface_endpoint(movement.get("to")) != INTERFACE_ID or interface_endpoint(movement.get("from")) is None:
        return None
    try:
        run = outbound_runtime(movement)["run"]
        artifact = movement.get("artifact")
        content = content_hexdigest(artifact.get("content")) if isinstance(artifact, dict) else None
    except (ContractError, ValueError):
        return None, None
    return content, run


class OutboundReader:
    def __init__(self):
        self.identity = None
        self.offset = 0

    def locate(self, next_seq):
        try:
            connection, _ = open_index()
            try:
                row = connection.execute("SELECT start_offset FROM movements WHERE seq = ?", (next_seq,)).fetchone()
                if row is None or row[0] is None:
                    row = connection.execute("SELECT end_offset FROM movements WHERE seq = ?", (next_seq - 1,)).fetchone()
            finally:
                connection.close()
        except (OSError, RuntimeError, ValueError, sqlite3.DatabaseError) as error:
            log(f"the ledger index is unavailable, so outbound catch-up reads the whole ledger: {error}")
            return 0
        if row is None or row[0] is None:
            return 0
        offset = int(row[0])
        for movement, _, _ in ledger_rows(offset):
            return offset if movement.get("seq") == next_seq else 0
        return offset

    def read(self, next_seq, limit=64):
        try:
            status = os.stat(MOVEMENTS_JSONL)
        except FileNotFoundError:
            return []
        identity = (status.st_dev, status.st_ino)
        if identity != self.identity or self.offset > status.st_size:
            self.identity, self.offset = identity, self.locate(next_seq)
        result, holding = [], False
        for movement, _, end in ledger_rows(self.offset):
            seq = contract.require_int(movement.get("seq"), "email ledger movement.seq", 0)
            found = own_outbound(movement) if seq >= next_seq else None
            if found is not None:
                result.append((seq, found[0], found[1]))
                holding = True
                if len(result) >= limit:
                    break
            elif not holding:
                self.offset = end
        return result


def content_hexdigest(content):
    if isinstance(content, list):
        if len(content) != 32 or any(isinstance(item, bool) or not isinstance(item, int) or not 0 <= item <= 255 for item in content):
            raise ValueError("email artifact content is not a 32-byte hash")
        return bytes(content).hex()
    if isinstance(content, str) and re.fullmatch(r"[0-9a-f]{64}", content):
        return content
    raise ValueError("email artifact content is not a 32-byte hash")


def read_blob(digest, maximum, name):
    content = read_bounded(os.path.join(BLOBS_DIR, digest), maximum, name)
    if octopus_content_hash(content) != digest:
        raise ValueError(f"email {name} failed content-address verification")
    return content


def artifact_payload(digest, maximum):
    manifest = contract.require_object(json.loads(read_blob(digest, CONFIG["outbound"]["max_manifest_bytes"], "artifact manifest")), "email artifact manifest")
    label = contract.require_string(manifest.get("label"), "email artifact label")
    payload_digest = manifest.get("payload_content_hash")
    if not isinstance(payload_digest, str) or not re.fullmatch(r"[0-9a-f]{64}", payload_digest):
        raise ValueError("email artifact manifest has no payload hash")
    return label, read_blob(payload_digest, maximum, "artifact payload")


def load_cursor():
    value = read_json(CURSOR_PATH, "ledger cursor")
    if value is None:
        return 0
    value = contract.require_keys(value, {"schema_version", "next_seq"}, set(), "email ledger cursor")
    if value["schema_version"] != 1:
        raise ValueError("email ledger cursor schema_version must be 1")
    return contract.require_int(value["next_seq"], "email ledger cursor.next_seq", 0)


def commit_cursor(next_seq):
    atomic_json(CURSOR_PATH, {"schema_version": 1, "next_seq": next_seq})


def ledger_next_seq():
    offset, next_seq = 0, 0
    try:
        connection, watermark = open_index()
        try:
            row = connection.execute("SELECT end_offset FROM movements WHERE seq = ?", (watermark - 1,)).fetchone()
        finally:
            connection.close()
        if watermark and row is not None and row[0] is not None:
            offset, next_seq = int(row[0]), watermark
    except (OSError, RuntimeError, ValueError, sqlite3.DatabaseError):
        pass
    for movement, _, _ in ledger_rows(offset):
        next_seq = max(next_seq, contract.require_int(movement.get("seq"), "email ledger movement.seq", 0) + 1)
    return next_seq


def account_identity(account):
    return {"address": account["address"].lower(), "imap": f"{account['imap']['host']}:{account['imap']['port']}"}


def load_state(account):
    value = read_json(STATE_PATH, "mailbox state")
    fresh = {"schema_version": 1, "account": account_identity(account), "mailboxes": {}, "admitted": []}
    if value is None:
        return fresh
    value = contract.require_keys(value, {"schema_version", "account", "mailboxes", "admitted"}, set(), "email mailbox state")
    if value["schema_version"] != 1:
        raise ValueError("email mailbox state schema_version must be 1")
    if value["account"] != account_identity(account):
        return fresh
    for mailbox, entry in contract.require_object(value["mailboxes"], "email mailbox state.mailboxes").items():
        entry = contract.require_keys(entry, {"uidvalidity", "last_uid", "checked_at"}, set(), f"email mailbox state {mailbox}")
        contract.require_int(entry["uidvalidity"], f"email mailbox state {mailbox}.uidvalidity", 1)
        contract.require_int(entry["last_uid"], f"email mailbox state {mailbox}.last_uid", 0)
        contract.require_string(entry["checked_at"], f"email mailbox state {mailbox}.checked_at")
    contract.require_string_list(value["admitted"], "email mailbox state.admitted")
    return value


def update_state(account, mutate):
    with _STATE_LOCK:
        state = load_state(account)
        result = mutate(state)
        state["admitted"] = state["admitted"][-CONFIG["intake"]["message_key_retention"]:]
        atomic_json(STATE_PATH, state)
        return result


def own_message_ids():
    with _STATE_LOCK:
        value = read_json(SENT_IDS_PATH, "sent message ids")
        return set(contract.require_string_list(value["ids"], "email sent message ids")) if value else set()


def remember_own_message_id(message_id):
    with _STATE_LOCK:
        ids = [item for item in own_message_ids_list() if item != message_id] + [message_id]
        atomic_json(SENT_IDS_PATH, {"schema_version": 1, "ids": ids[-SENT_ID_RETENTION:]})


def own_message_ids_list():
    value = read_json(SENT_IDS_PATH, "sent message ids")
    return list(contract.require_string_list(value["ids"], "email sent message ids")) if value else []


def now_iso():
    return datetime.datetime.now(datetime.timezone.utc).replace(microsecond=0).isoformat()


class AttachmentStore:
    def __init__(self, root):
        self.root = root

    def ensure(self):
        os.makedirs(self.root, mode=0o700, exist_ok=True)
        current = os.lstat(self.root)
        if stat.S_ISLNK(current.st_mode) or not stat.S_ISDIR(current.st_mode):
            raise PermissionError("the email attachment store is not a real directory")

    def put(self, data, filename, digest):
        directory = os.path.join(self.root, digest)
        os.makedirs(directory, mode=0o700, exist_ok=True)
        path = os.path.join(directory, filename)
        try:
            current = os.lstat(path)
            if stat.S_ISREG(current.st_mode) and current.st_size == len(data):
                return path
        except FileNotFoundError:
            pass
        atomic_bytes(path, data)
        return path

    def read(self, requested, maximum):
        if not isinstance(requested, str) or not os.path.isabs(requested):
            raise ContractError("attachment path must be an absolute path from an email event")
        root = os.path.realpath(self.root)
        path = os.path.realpath(requested)
        relative = os.path.relpath(path, root)
        parts = relative.split(os.sep)
        if relative.startswith("..") or len(parts) != 2 or not re.fullmatch(r"[0-9a-f]{64}", parts[0]):
            raise ContractError("attachment path is not a stored email attachment")
        data = read_bounded(path, maximum, "stored attachment")
        if hashlib.sha256(data).hexdigest() != parts[0]:
            raise ContractError("stored attachment changed since it was received")
        return parts[1], data


class Mailbox:
    def __init__(self, config, account, store):
        self.config = config
        self.account = account
        self.store = store
        transport = config["transport"]
        self.connect_timeout = transport["connect_timeout_ms"] / 1000
        self.command_timeout = transport["command_timeout_ms"] / 1000

    def imap(self):
        return ImapSession.open(self.account["imap"], self.account["address"], self.account["password"], self.connect_timeout, self.command_timeout)

    def smtp(self):
        return SmtpSession.open(self.account["smtp"], self.account["address"], self.account["password"], self.connect_timeout, self.command_timeout)

    def resolve_mailbox(self, session, name):
        if not name.startswith("\\"):
            return name
        if name not in contract.SPECIAL_USE:
            raise ContractError(f"{name} is not a special mailbox; use one of {', '.join(sorted(contract.SPECIAL_USE))}")
        found = session.special_mailbox(name)
        if found is None:
            raise ContractError(f"this mailbox has no {name} folder; list mailboxes and name one")
        return found

    def select_ref(self, session, ref, readonly):
        status = session.select(self.resolve_mailbox(session, ref["mailbox"]), readonly=readonly)
        if status["uidvalidity"] != ref["uidvalidity"]:
            raise ContractError(f"{ref['mailbox']} was renumbered since this message was seen; search for it again")
        return status

    def locate(self, session, target, mailbox):
        kind, value = target
        if kind == "ref":
            self.select_ref(session, value, readonly=True)
            return value["mailbox"], value["uidvalidity"], value["uid"]
        if mailbox:
            candidates = [self.resolve_mailbox(session, mailbox)]
        else:
            specials = {flag: row["name"] for row in reversed(session.mailboxes()) for flag in contract.special_use_flags(row["flags"])}
            candidates = ["INBOX"] + [specials[flag] for flag in ("\\All", "\\Sent", "\\Archive") if flag in specials]
        for name in dict.fromkeys(candidates):
            uid = session.find_message_id(name, value)
            if uid:
                return name, session.select(name, readonly=True)["uidvalidity"], uid
        raise ContractError(f"no message with Message-ID {value} was found")

    def load_original(self, session, target, mailbox, max_body_chars):
        name, uidvalidity, uid = self.locate(session, target, mailbox)
        raw = session.fetch_raw(uid, self.config["content"]["max_message_bytes"])
        if raw is None:
            raise ContractError("the original message is no longer in the mailbox")
        parsed = contract.parse_message(raw, self.config["content"], max_body_chars)
        return {**parsed, "ref": {"mailbox": name, "uidvalidity": uidvalidity, "uid": uid}}, raw

    def attachment(self, spec, index):
        limit = self.config["outbound"]["max_total_attachment_bytes"]
        if "text" in spec or "content_base64" in spec:
            return contract.inline_attachment(spec, index)
        if "path" in spec:
            stored_name, data = self.store.read(spec["path"], limit)
            filename = contract.safe_filename(spec.get("filename") or stored_name, spec.get("media_type", ""))
        else:
            reference = spec["artifact"].strip()
            label, _, digest = reference.rpartition(":")
            if not re.fullmatch(r"[0-9a-f]{64}", digest):
                raise ContractError(f"attachments[{index}].artifact must be an artifact id or content hash")
            try:
                manifest_label, data = artifact_payload(digest, limit)
            except FileNotFoundError:
                raise ContractError(f"attachments[{index}].artifact is not a known artifact") from None
            except ValueError as error:
                raise ContractError(f"attachments[{index}].artifact could not be read: {error}") from None
            if label and label != manifest_label:
                raise ContractError(f"attachments[{index}].artifact is labelled {manifest_label}, not {label}")
            filename = contract.safe_filename(spec.get("filename") or manifest_label, spec.get("media_type", ""))
        media_type = spec.get("media_type") or mimetypes.guess_type(filename)[0] or "application/octet-stream"
        return {"filename": filename, "media_type": media_type.lower(), "data": data}

    def message_summary(self, mailbox, uidvalidity, summary):
        headers = contract.header_summary(contract.parse_header_block(summary["headers"]))
        return {
            "ref": {"mailbox": mailbox, "uidvalidity": uidvalidity, "uid": summary["uid"]},
            "message_id": headers["message_id"],
            "from": headers["from"],
            "to": headers["to"],
            "cc": headers["cc"],
            "subject": headers["subject"],
            "date": headers["date"],
            "received_at": summary["internal_date"],
            "flags": summary["flags"],
            "size": summary["size"],
        }

    def search(self, args):
        outbound = self.config["outbound"]
        session = self.imap()
        try:
            name = self.resolve_mailbox(session, args.get("mailbox", "INBOX"))
            status = session.select(name, readonly=True)
            uids = sorted(session.search(contract.search_criteria(args)), reverse=True)
            chosen = uids[: args.get("limit", outbound["default_search_results"])]
            messages = []
            for uid in chosen:
                summary = session.fetch_summary(uid, contract.SUMMARY_HEADERS, self.config["content"]["max_header_bytes"])
                if summary is not None:
                    messages.append(self.message_summary(name, status["uidvalidity"], summary))
        finally:
            session.close()
        return {"mailbox": name, "uidvalidity": status["uidvalidity"], "total": len(uids), "messages": messages}

    def fetch(self, args):
        session = self.imap()
        try:
            name, uidvalidity, uid = self.locate(session, args["target"], args.get("mailbox"))
            raw = session.fetch_raw(uid, self.config["content"]["max_message_bytes"])
        finally:
            session.close()
        if raw is None:
            raise ContractError("the message is no longer in the mailbox")
        parsed = contract.parse_message(raw, self.config["content"], self.config["outbound"]["max_fetch_body_chars"], self.store.put)
        message = {key: parsed[key] for key in ("message_id", "in_reply_to", "references", "date", "from", "reply_to", "to", "cc", "subject", "body", "attachments", "omitted_attachments", "inline_parts")}
        return {"message": {"ref": {"mailbox": name, "uidvalidity": uidvalidity, "uid": uid}, **message}}, parsed["media"]

    def mailboxes(self):
        session = self.imap()
        try:
            rows = session.mailboxes()
        finally:
            session.close()
        return {"mailboxes": [{"name": row["name"], "special_use": contract.special_use_flags(row["flags"])} for row in rows]}

    def mark(self, args):
        refs = args["refs"]
        session = self.imap()
        try:
            self.select_ref(session, refs[0], readonly=False)
            uids = [ref["uid"] for ref in refs]
            for key, flag in (("seen", "\\Seen"), ("flagged", "\\Flagged")):
                if key in args:
                    session.store_flags(uids, args[key], [flag])
        finally:
            session.close()
        return {"mailbox": refs[0]["mailbox"], "uids": uids, **{key: args[key] for key in ("seen", "flagged") if key in args}}

    def move(self, args):
        refs = args["refs"]
        session = self.imap()
        try:
            self.select_ref(session, refs[0], readonly=False)
            destination = self.resolve_mailbox(session, args["to_mailbox"])
            outcome = session.move([ref["uid"] for ref in refs], destination)
        finally:
            session.close()
        return {"from_mailbox": refs[0]["mailbox"], "to_mailbox": destination, "uids": [ref["uid"] for ref in refs], **outcome}

    def prepare_send(self, operation, args, message_id):
        original, raw = None, None
        if operation != "message.send":
            session = self.imap()
            try:
                original, raw = self.load_original(session, args["target"], args.get("mailbox"), self.config["outbound"]["max_body_chars"] // 2)
            finally:
                session.close()
        limit = self.config["outbound"]["max_total_attachment_bytes"]
        attachments = []
        if operation == "message.forward" and args.get("include_attachments", True):
            attachments.extend(contract.original_attachments(raw, limit))
        attachments.extend(self.attachment(spec, index) for index, spec in enumerate(args["attachments"]))
        if sum(len(item["data"]) for item in attachments) > limit:
            raise ContractError(f"attachments exceed {limit} bytes in total")
        message, recipients, summary = contract.compose_message(operation, args, self.account, self.config["outbound"], original, attachments, message_id, datetime.datetime.now(datetime.timezone.utc))
        if original is not None:
            summary["original"] = original["ref"]
        return contract.serialize_message(message), recipients, summary

    def submit(self, data, recipients, before_commit):
        session = self.smtp()
        try:
            return session.submit(self.account["address"], recipients, data, before_commit)
        finally:
            session.close()

    def save_sent_copy(self, message_id, data):
        mode = self.account["sent_copy"]
        if mode != "append":
            return mode
        session = self.imap()
        try:
            sent = session.special_mailbox("\\Sent")
            if sent is None:
                return "no Sent mailbox"
            if session.find_message_id(sent, message_id) is None:
                session.append(sent, data, "(\\Seen)")
            return "appended"
        finally:
            session.close()

    def confirm_sent(self, message_id, recipients):
        session = self.imap()
        try:
            places = [place for place in (session.special_mailbox("\\Sent"),) if place]
            if self.account["address"].lower() in {item.lower() for item in recipients}:
                places.append("INBOX")
            for attempt in range(3 if places else 0):
                if attempt:
                    time.sleep(2)
                for place in places:
                    if session.find_message_id(place, message_id):
                        return place
        finally:
            session.close()
        return None


def human_failure(error, account):
    if isinstance(error, MailError):
        message = str(error)
        if error.server_text:
            message += f" ({error.server_text})"
        if error.kind == "authentication":
            message += f". {account['sign_in_help']}"
        return FAILURE_CLASSES.get(error.kind, "server_error"), message, error.retryable
    return "rejected", str(error), False


INDETERMINATE = "The mail server stopped answering while it was accepting this message, and the message could not be found afterwards. It may or may not have been sent, so it was not sent again; check with email.search_messages before sending it again."


class Outbound:
    def __init__(self, mailbox):
        self.mailbox = mailbox
        self.config = mailbox.config
        self.account = mailbox.account

    def journal_path(self, key):
        payload_paths(key)
        return os.path.join(REQUEST_DIR, f"{key}.json")

    def load_journal(self, key):
        value = read_json(self.journal_path(key), f"request journal {key}")
        if value is None:
            return None
        value = contract.require_keys(value, {"schema_version", "source_movement_seq", "run", "operation", "correlation_id", "phase", "message_id"}, {"recipients", "planned"}, f"email request journal {key}")
        if value["schema_version"] != 1 or value["phase"] not in {"prepared", "committing"}:
            raise ValueError(f"email request journal {key} is malformed")
        return value

    def save(self, key, journal):
        atomic_json(self.journal_path(key), sanitize(journal))

    def reply(self, label, key, body):
        try:
            hand_json(label, body, key)
        except DeliveryRefused as refusal:
            if refusal.transient():
                raise Paused(str(refusal)) from None
            log(f"the answer {key} was refused by the runtime: {refusal.reason}")
        try:
            os.unlink(self.journal_path(key))
        except FileNotFoundError:
            pass

    def process(self, source_seq, content, run):
        if content is None or run is None:
            log(f"outbound movement {source_seq} carries no artifact or run, so it is not an operation request")
            return
        catalog = load_operations()
        label = catalog["result_label"]
        try:
            request_label, payload = artifact_payload(content, self.config["outbound"]["max_payload_bytes"])
        except (ContractError, ValueError, FileNotFoundError) as error:
            log(f"outbound movement {source_seq} cannot be read as an artifact, so it is left unanswered: {error}")
            return
        if request_label != catalog["request_label"]:
            log(f"email answers only {catalog['request_label']} artifacts; outbound {request_label} is not an operation request")
            return
        key = f"result-{source_seq}-{content[:16]}"
        if materialized(key):
            self.reply(label, key, None)
            return
        try:
            spec, args, correlation_id = parse_request(payload, catalog)
        except RequestError as error:
            self.reply(label, key, answer(label, error.operation, error.correlation_id, error={"code": error.code, "message": str(error)}))
            return

        def respond(result=None, error=None, media=None):
            self.reply(label, key, answer(label, spec["id"], correlation_id, result=result, error=error, media=media))

        try:
            journal = self.load_journal(key)
        except (ContractError, ValueError) as error:
            path = self.journal_path(key)
            os.replace(path, f"{path}.unreadable")
            respond(error={"code": "request_journal_unreadable", "message": f"the delivery journal for this request could not be read ({error}); it was not executed again", "retryable": False})
            return
        if journal is not None and journal["phase"] == "committing":
            self.recover_commit(journal, respond)
            return
        operation = spec["handler_config"]["operation"]
        try:
            args = contract.normalize_arguments(operation, args, self.config)
        except ContractError as error:
            respond(error={"code": "invalid_arguments", "message": str(error), "retryable": False})
            return
        if operation in contract.SENDING_OPERATIONS:
            journal = {"schema_version": 1, "source_movement_seq": source_seq, "run": run, "operation": spec["id"], "correlation_id": correlation_id, "phase": "prepared", "message_id": contract.operation_message_id(INTERFACE_ID, source_seq, content, self.account["domain"])}
            remember_own_message_id(journal["message_id"])
            self.save(key, journal)
        try:
            result, media = self.execute(key, journal, operation, args)
        except CommitUncertain as error:
            log(f"outbound {source_seq}: {error}")
            self.recover_commit(journal, respond)
            return
        except (ContractError, MailError) as error:
            failure_class, message, retryable = human_failure(error, self.account)
            respond(error={"code": failure_class, "message": message, "retryable": retryable})
            return
        respond(result=result, media=media)

    def execute(self, key, journal, operation, args):
        if operation == "message.search":
            return self.mailbox.search(args), None
        if operation == "message.fetch":
            return self.mailbox.fetch(args)
        if operation == "mailbox.list":
            return self.mailbox.mailboxes(), None
        if operation == "message.mark":
            return self.mailbox.mark(args), None
        if operation == "message.move":
            return self.mailbox.move(args), None
        data, recipients, summary = self.mailbox.prepare_send(operation, args, journal["message_id"])

        def before_commit():
            journal.update({"phase": "committing", "recipients": recipients, "planned": summary})
            self.save(key, journal)

        outcome = self.mailbox.submit(data, recipients, before_commit)
        try:
            sent_copy = self.mailbox.save_sent_copy(journal["message_id"], data)
        except (MailError, ContractError) as error:
            sent_copy = f"not saved: {error}"
        return {**summary, "accepted": outcome["accepted"], "refused": outcome["refused"], "smtp_reply": outcome["reply"], "sent_copy": sent_copy}, None

    def recover_commit(self, journal, respond):
        try:
            place = self.mailbox.confirm_sent(journal["message_id"], journal.get("recipients", []))
        except MailError as error:
            log(f"outbound {journal['source_movement_seq']} could not be confirmed: {error}")
            place = None
        if place:
            respond(result={**journal.get("planned", {}), "confirmed_in": place, "recovered": True})
        else:
            respond(error={"code": "indeterminate_external_effect", "message": INDETERMINATE, "retryable": False})


def outbound_loop(outbound):
    next_seq = load_cursor()
    reader = OutboundReader()
    delay = 0.25
    while True:
        try:
            rows = reader.read(next_seq)
            for source_seq, content, run in rows:
                outbound.process(source_seq, content, run)
                next_seq = source_seq + 1
                commit_cursor(next_seq)
            delay = 0.25
        except Paused as pause:
            log(f"outbound paused: {pause}")
            delay = min(delay * 2, 30)
        except Exception as error:
            log(f"outbound movement {next_seq} retained after error: {error}")
            delay = min(delay * 2, 30)
        time.sleep(max(delay, CONFIG["transport"]["outbound_poll_ms"] / 1000))


def message_key(message, sender):
    material = message.get("message_id") or json.dumps([sender, message.get("date"), message.get("subject")], ensure_ascii=False)
    return hashlib.sha256(f"{sender.lower()}\0{material}".encode("utf-8")).hexdigest()


def event_key(stem):
    event = json.loads(read_bounded(payload_paths(stem)[0], io_limits()["max_payload_bytes"], "artifact payload"))
    return message_key(event["message"], event["sender"]["address"])


class Watcher:
    def __init__(self, mailbox, name):
        self.mailbox = mailbox
        self.name = name
        self.config = mailbox.config
        self.account = mailbox.account

    def stem(self, uidvalidity, uid):
        identity = f"{self.account['address'].lower()}\0{self.account['imap']['host']}\0{self.name}\0{uidvalidity}\0{uid}"
        return "inbound-" + hashlib.sha256(identity.encode("utf-8")).hexdigest()[:48]

    def run(self):
        transport = self.config["transport"]
        delay, sign_in_delay = transport["reconnect_backoff_ms"], transport["sign_in_retry_ms"]
        while True:
            session = None
            try:
                session = self.mailbox.imap()
                delay, sign_in_delay = transport["reconnect_backoff_ms"], transport["sign_in_retry_ms"]
                self.watch(session)
            except MailError as error:
                if error.kind == "authentication":
                    log(f"{self.name}: sign-in failed ({error}; {error.server_text}); retrying in {sign_in_delay // 1000}s. {self.account['sign_in_help']}")
                    time.sleep(sign_in_delay / 1000)
                    sign_in_delay = min(sign_in_delay * 2, transport["sign_in_retry_cap_ms"])
                    continue
                log(f"{self.name}: {error} {error.server_text}; reconnecting in {delay // 1000}s")
            except Paused as pause:
                log(f"{self.name}: delivery paused ({pause})")
            except Exception as error:
                log(f"{self.name}: intake stopped by {type(error).__name__}: {error}; restarting")
            finally:
                if session is not None:
                    session.close()
            time.sleep(delay / 1000)
            delay = min(delay * 2, transport["reconnect_backoff_cap_ms"])

    def watch(self, session):
        intake = self.config["intake"]
        while True:
            status = session.select(self.name, readonly=not intake["mark_seen"])
            more = self.catch_up(session, status)
            if more:
                continue
            if intake["use_idle"] and session.has("IDLE"):
                session.idle(self.config["transport"]["idle_refresh_ms"] / 1000)
            else:
                time.sleep(self.config["transport"]["poll_interval_ms"] / 1000)

    def baseline(self, session, status, previous):
        if previous is not None:
            since = (datetime.datetime.fromisoformat(previous["checked_at"]) - datetime.timedelta(days=1)).strftime("%d-%b-%Y")
            recent = sorted(session.search([("SINCE", since)]))
            return (recent[0] - 1) if recent else max((status["uidnext"] or 1) - 1, 0)
        last = (status["uidnext"] - 1) if status["uidnext"] else (session.last_uid() if status["exists"] else 0)
        backlog = self.config["intake"]["backlog_messages"]
        if backlog and status["exists"]:
            existing = sorted(session.search([("ALL", None)]))
            return existing[-backlog - 1] if len(existing) > backlog else 0
        return last

    def catch_up(self, session, status):
        account = self.account
        state = load_state(account)
        entry = state["mailboxes"].get(self.name)
        if entry is None or entry["uidvalidity"] != status["uidvalidity"]:
            start = self.baseline(session, status, entry)
            if entry is not None:
                log(f"{self.name} was renumbered by the server; rechecking mail since {entry['checked_at']}")

            def reset(current):
                current["mailboxes"][self.name] = {"uidvalidity": status["uidvalidity"], "last_uid": start, "checked_at": now_iso()}

            update_state(account, reset)
            entry = {"uidvalidity": status["uidvalidity"], "last_uid": start}
        limit = self.config["intake"]["max_messages_per_cycle"]
        uids = session.uids_after(entry["last_uid"])
        handed, ignored = 0, {}
        for uid in uids[:limit]:
            outcome = self.process(session, status["uidvalidity"], uid)
            if outcome == "handed":
                handed += 1
            else:
                ignored[outcome] = ignored.get(outcome, 0) + 1

            def advance(current, uid=uid):
                current["mailboxes"][self.name] = {"uidvalidity": status["uidvalidity"], "last_uid": uid, "checked_at": now_iso()}

            update_state(account, advance)
        if uids:
            detail = ", ".join(f"{reason}: {count}" for reason, count in sorted(ignored.items()))
            log(f"{self.name}: {min(len(uids), limit)} new, {handed} handed to tasks" + (f"; not handed ({detail})" if detail else ""))
        return len(uids) > limit

    def process(self, session, uidvalidity, uid):
        stem = self.stem(uidvalidity, uid)
        label = self.config["intake"]["event_label"]
        if materialized(stem):
            return self.hand(stem, label, None, event_key(stem), session, uid)
        summary = session.fetch_summary(uid, contract.SUMMARY_HEADERS, self.config["content"]["max_header_bytes"])
        if summary is None:
            return "removed before reading"
        headers = contract.parse_header_block(summary["headers"])
        decision = contract.admission(headers, self.account, self.config, own_message_ids(), summary["labels"])
        if not decision["admit"]:
            return decision["reason"]
        key = message_key(decision["summary"], decision["sender"]["address"])
        if key in load_state(self.account)["admitted"]:
            return "already handed"
        content = self.config["content"]
        if summary["size"] > content["max_message_bytes"]:
            parsed = {**decision["summary"], "attachments": [], "omitted_attachments": 0, "inline_parts": 0, "body": {"text": "", "source": "none", "truncated": True, "omitted": f"the message is {summary['size']} bytes, above the {content['max_message_bytes']}-byte limit"}}
        else:
            raw = session.fetch_raw(uid, content["max_message_bytes"])
            if raw is None:
                return "removed before reading"
            parsed = contract.parse_message(raw, content, content["max_body_chars"], self.mailbox.store.put)
        ref = {"mailbox": self.name, "uidvalidity": uidvalidity, "uid": uid}
        event = contract.inbound_event(self.account, ref, decision, parsed, summary["flags"], summary["internal_date"])
        return self.hand(stem, label, event, key, session, uid)

    def hand(self, stem, label, event, key, session, uid):
        try:
            hand_json(label, event, stem)
        except DeliveryRefused as refusal:
            if refusal.transient():
                raise Paused(str(refusal)) from None
            return f"refused by the runtime ({refusal.reason})"

        def admit(current):
            if key not in current["admitted"]:
                current["admitted"].append(key)

        update_state(self.account, admit)
        if self.config["intake"]["mark_seen"]:
            session.store_flags([uid], True, ["\\Seen"])
        return "handed"


CONFIG = None


def main():
    global CONFIG
    os.umask(0o077)
    prepare_directories()
    CONFIG = load_config()
    load_operations()
    if read_json(CURSOR_PATH, "ledger cursor") is None:
        next_seq = ledger_next_seq()
        commit_cursor(next_seq)
        log(f"first start: email operations are handled from ledger position {next_seq} on")
    account = contract.resolve_account(CONFIG, os.environ)
    register_secret(account)
    io_limits()
    store = AttachmentStore(attachment_root())
    store.ensure()
    mailbox = Mailbox(CONFIG, account, store)
    threading.Thread(target=_runtime_verdict_reader, daemon=True).start()
    threading.Thread(target=outbound_loop, args=(Outbound(mailbox),), daemon=True).start()
    for name in CONFIG["intake"]["mailboxes"]:
        threading.Thread(target=Watcher(mailbox, name).run, daemon=True).start()
    write_frame({"kind": "ready"})
    log(f"watching {', '.join(CONFIG['intake']['mailboxes'])} for {account['address']} through {account['imap']['host']}")
    while True:
        with _VERDICT_CONDITION:
            _VERDICT_CONDITION.wait(timeout=5)
            if _VERDICT_INPUT_ERROR is not None:
                raise RuntimeError(f"runtime input closed: {_VERDICT_INPUT_ERROR}")


if __name__ == "__main__":
    try:
        main()
    except Exception as error:
        log(f"stopped: {error}")
        raise SystemExit(1)
