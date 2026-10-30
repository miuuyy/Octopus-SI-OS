#!/usr/bin/env python3
import base64
import hashlib
import json
import os
import re
import shutil
import sqlite3
import stat
import sys
import threading
import time
import urllib.parse

from octopus_content_hash import octopus_content_hash
from octopus_operation_catalog import RequestError
from octopus_operation_catalog import answer
from octopus_operation_catalog import load_catalog
from octopus_operation_catalog import parse_request

from slack_contract import CHANNEL_RE
from slack_contract import TEAM_RE
from slack_contract import TS_RE
from slack_contract import USER_RE
from slack_contract import envelope_key
from slack_contract import env_flag
from slack_contract import embed_image
from slack_contract import env_ids
from slack_contract import event_coordinates
from slack_contract import image_room
from slack_contract import require_int
from slack_contract import require_keys
from slack_contract import require_object
from slack_contract import require_string
from slack_contract import rich_text
from slack_contract import safe_blocks
from slack_contract import slack_mrkdwn
from slack_contract import validate_config
from slack_contract import validate_environment
from slack_contract import validate_handlers
from slack_wire import SlackApi
from slack_wire import SlackWireError
from slack_wire import WebSocket


INTERFACE_ID = os.environ["OCTOPUS_INTERFACE_ID"]
INTERFACE_DIR = os.environ["OCTOPUS_INTERFACE_DIR"]
VAR_DIR = os.environ["OCTOPUS_VAR_DIR"]
MOVEMENTS_JSONL = os.environ["OCTOPUS_MOVEMENTS_JSONL"]
BLOBS_DIR = os.environ["OCTOPUS_BLOBS_DIR"]
LEDGER_INDEX_SQLITE = os.environ["OCTOPUS_LEDGER_INDEX_SQLITE"]
CONFIG_PATH = os.path.join(INTERFACE_DIR, "config.json")
DESCRIPTOR_PATH = os.path.join(INTERFACE_DIR, "interface.json")
CURSOR_PATH = os.path.join(VAR_DIR, "ledger-cursor.json")
RAW_DIR = os.path.join(VAR_DIR, "raw-events")
PENDING_DIR = os.path.join(VAR_DIR, "pending-events")
PAYLOAD_DIR = os.path.join(VAR_DIR, "payloads")
MEDIA_DIR = os.path.join(VAR_DIR, "media")
REQUEST_DIR = os.path.join(VAR_DIR, "operation-requests")
CALLBACK_DIR = os.path.join(VAR_DIR, "callback-handles")
STATE_PATH = os.path.join(VAR_DIR, "slack-state.json")
CONFIG_MAX_BYTES = 1024 * 1024

os.umask(0o077)


class ArtifactDeliveryRefused(RuntimeError):
    def __init__(self, verdict):
        self.verdict = verdict
        super().__init__(f"runtime refused Slack artifact: {verdict!r}")


def ensure_private_directory(path):
    path = os.path.abspath(path)
    os.makedirs(path, mode=0o700, exist_ok=True)
    current = os.lstat(path)
    if stat.S_ISLNK(current.st_mode) or not stat.S_ISDIR(current.st_mode):
        raise PermissionError(f"Slack private directory is not a real directory: {path}")
    if current.st_uid != os.getuid():
        raise PermissionError(f"Slack private directory has a different owner: {path}")
    if stat.S_IMODE(current.st_mode) != 0o700:
        os.chmod(path, 0o700)
    return path


for directory in (VAR_DIR, RAW_DIR, PENDING_DIR, PAYLOAD_DIR, MEDIA_DIR, REQUEST_DIR, CALLBACK_DIR):
    ensure_private_directory(directory)

_STDOUT_LOCK = threading.Lock()
_VERDICT_CONDITION = threading.Condition()
_VERDICTS = {}
_VERDICT_INPUT_ERROR = None
_NEXT_REQUEST_ID = 1
_STATE_LOCK = threading.RLock()
_LEDGER_LOCK = threading.RLock()
_LEDGER_FALLBACK_OFFSET = 0
_LEDGER_FALLBACK_SIZE = 0
_LEDGER_FALLBACK_IDENTITY = None
_LEDGER_INDEX_WARNING = None
_CONFIG = None


def open_private_read(path):
    if not isinstance(path, str) or not os.path.isabs(path):
        raise PermissionError("Slack private file path must be absolute")
    path = os.path.abspath(path)
    before = os.lstat(path)
    if stat.S_ISLNK(before.st_mode) or not stat.S_ISREG(before.st_mode):
        raise PermissionError(f"Slack private path is not a regular file: {path}")
    if before.st_uid != os.getuid() or stat.S_IMODE(before.st_mode) != 0o600:
        raise PermissionError(f"Slack private path must be owner-only 0600: {path}")
    descriptor = os.open(path, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0))
    try:
        current = os.fstat(descriptor)
        linked = os.lstat(path)
        if not stat.S_ISREG(current.st_mode) or current.st_uid != os.getuid() or stat.S_IMODE(current.st_mode) != 0o600:
            raise PermissionError(f"Slack private path changed permissions: {path}")
        if stat.S_ISLNK(linked.st_mode) or (linked.st_dev, linked.st_ino) != (current.st_dev, current.st_ino) or (before.st_dev, before.st_ino) != (current.st_dev, current.st_ino):
            raise PermissionError(f"Slack private path changed while opening: {path}")
        return os.fdopen(descriptor, "rb")
    except Exception:
        os.close(descriptor)
        raise


def sensitive_values(config=None):
    values = set()
    if isinstance(config, dict):
        transport = config.get("transport") if isinstance(config.get("transport"), dict) else {}
        environment = config.get("environment") if isinstance(config.get("environment"), dict) else {}
        for env_name in (environment.get("bot_token_env"), environment.get("app_token_env"), transport.get("proxy_env"), transport.get("socket_url_env")):
            raw = os.environ.get(env_name, "") if isinstance(env_name, str) else ""
            if raw:
                values.add(raw)
                values.update(part.strip() for part in raw.split(",") if part.strip())
    return {value for value in values if len(value) >= 4}


def sanitize_external_error(value, config=None):
    if isinstance(value, dict):
        return {sanitize_external_error(key, config): sanitize_external_error(item, config) for key, item in value.items()}
    if isinstance(value, list):
        return [sanitize_external_error(item, config) for item in value]
    if isinstance(value, tuple):
        return tuple(sanitize_external_error(item, config) for item in value)
    if isinstance(value, BaseException):
        value = str(value)
    if not isinstance(value, str):
        return value
    tokens = set(sensitive_values(config))
    for token in tuple(tokens):
        tokens.add(urllib.parse.quote(token, safe=""))
        tokens.add(urllib.parse.quote_plus(token, safe=""))
        tokens.add(base64.b64encode(token.encode("utf-8")).decode("ascii"))
    sanitized = value
    for token in sorted(tokens, key=len, reverse=True):
        sanitized = sanitized.replace(token, "[secret-redacted]")
    sanitized = re.sub(r"xox[baprs]-[0-9A-Za-z-]+", "[slack-token-redacted]", sanitized)
    sanitized = re.sub(r"xapp-[0-9A-Za-z-]+", "[slack-token-redacted]", sanitized)
    sanitized = re.sub(r"https://hooks\.slack(?:-gov)?\.com/services/[^\s\"']+", "[slack-callback-redacted]", sanitized)
    sanitized = re.sub(r"wss://[^\s\"']+", "[slack-socket-url-redacted]", sanitized)
    sanitized = re.sub(r"(?i)(authorization\s*[:=]\s*(?:bearer|basic)\s+)[^\s,;]+", r"\1[secret-redacted]", sanitized)
    sanitized = re.sub(r"(https?://)([^/@\s]+)@", r"\1[credentials-redacted]@", sanitized)
    return sanitized


def stderr(config, message):
    sys.stderr.write(sanitize_external_error(message, config) + "\n")
    sys.stderr.flush()


_DIAGNOSTIC_WINDOWS = {}
DIAGNOSTIC_WINDOW_S = 3600


def throttled_stderr(config, key, message):
    now = time.monotonic()
    started, suppressed = _DIAGNOSTIC_WINDOWS.get(key, (None, 0))
    if started is not None and now - started < DIAGNOSTIC_WINDOW_S:
        _DIAGNOSTIC_WINDOWS[key] = (started, suppressed + 1)
        return
    _DIAGNOSTIC_WINDOWS[key] = (now, 0)
    stderr(config, f"{message} ({suppressed} similar suppressed)" if suppressed else message)


def read_private_config():
    descriptor = require_object(json.loads(read_bounded_regular(DESCRIPTOR_PATH, CONFIG_MAX_BYTES, "interface descriptor")), "Slack interface descriptor")
    if descriptor.get("id") != INTERFACE_ID:
        raise ValueError("Slack interface descriptor identity does not match its folder")
    driver = require_object(descriptor.get("interface_driver_config"), "Slack interface_driver_config")
    return require_object(driver.get("private_config"), "Slack interface private_config")


def load_config():
    config = require_object(json.loads(read_bounded_regular(CONFIG_PATH, CONFIG_MAX_BYTES, "interface config")), "Slack config")
    config["environment"] = validate_environment(read_private_config().get("environment"))
    return validate_config(config)


def load_operations():
    return validate_handlers(load_catalog(INTERFACE_DIR, read_private_config(), "slack"))


def atomic_bytes(path, payload):
    parent = os.path.dirname(path)
    ensure_private_directory(parent)
    temporary = os.path.join(parent, f".{os.path.basename(path)}.{os.getpid()}.{time.time_ns()}.tmp")
    descriptor = None
    try:
        descriptor = os.open(temporary, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
        with os.fdopen(descriptor, "wb") as handle:
            descriptor = None
            handle.write(payload)
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(temporary, path)
        os.chmod(path, 0o600)
        directory = os.open(parent, os.O_RDONLY)
        try:
            os.fsync(directory)
        finally:
            os.close(directory)
    finally:
        if descriptor is not None:
            os.close(descriptor)
        try:
            os.unlink(temporary)
        except FileNotFoundError:
            pass


def atomic_json(path, value, indent=None):
    atomic_bytes(path, json.dumps(value, ensure_ascii=False, sort_keys=True, indent=indent).encode("utf-8"))


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


def start_runtime_verdict_reader():
    threading.Thread(target=_runtime_verdict_reader, daemon=True).start()


def signal_ready():
    with _STDOUT_LOCK:
        sys.stdout.write(json.dumps({"kind": "ready"}, separators=(",", ":")) + "\n")
        sys.stdout.flush()


def hand_artifact(manifest_path, evidence=None):
    global _NEXT_REQUEST_ID
    with _VERDICT_CONDITION:
        request_id = _NEXT_REQUEST_ID
        _NEXT_REQUEST_ID += 1
    with _STDOUT_LOCK:
        sys.stdout.write(json.dumps({"kind": "arrival", "request_id": request_id, "artifact_manifest_path": manifest_path, "evidence": evidence or []}, separators=(",", ":")) + "\n")
        sys.stdout.flush()
    with _VERDICT_CONDITION:
        while request_id not in _VERDICTS and _VERDICT_INPUT_ERROR is None:
            _VERDICT_CONDITION.wait()
        if _VERDICT_INPUT_ERROR is not None:
            raise RuntimeError(f"runtime verdict input failed before Slack request {request_id}: {_VERDICT_INPUT_ERROR}")
        verdict = _VERDICTS.pop(request_id)
    admitted = verdict.get("admitted")
    if isinstance(admitted, dict) and isinstance(admitted.get("seq"), int) and not isinstance(admitted.get("seq"), bool):
        return admitted["seq"]
    raise ArtifactDeliveryRefused(verdict)


def artifact_paths(key):
    key = require_string(key, "Slack artifact key", maximum=512)
    if not re.fullmatch(r"[0-9A-Za-z._-]+", key):
        raise ValueError("Slack artifact key contains unsafe characters")
    return os.path.join(PAYLOAD_DIR, f"{key}.payload"), os.path.join(PAYLOAD_DIR, f"{key}.artifact.json")


def materialize_artifact(label, payload, key):
    payload_path, manifest_path = artifact_paths(key)
    if payload is not None:
        atomic_bytes(payload_path, payload)
    atomic_json(manifest_path, {"label": label, "payload_path": payload_path})
    return manifest_path


def encode(value, config):
    media = value.get("media")
    value = sanitize_external_error({name: item for name, item in value.items() if name != "media"}, config)
    if media is not None:
        value["media"] = media
    return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":"), allow_nan=False).encode("utf-8")


def materialized_artifact_id(manifest_path, config):
    manifest = require_keys(json.loads(read_bounded_regular(manifest_path, 1048576, "materialized artifact manifest")), {"label", "payload_path"}, {"payload_content_hash"}, "Slack materialized artifact manifest")
    label = require_string(manifest["label"], "Slack materialized artifact label")
    payload_path = require_string(manifest["payload_path"], "Slack materialized artifact payload_path")
    if not os.path.isabs(payload_path):
        raise ValueError("Slack materialized artifact payload_path must be absolute")
    payload = read_bounded_regular(payload_path, config["outbound"]["max_payload_bytes"], "materialized artifact payload")
    payload_hash = octopus_content_hash(payload)
    declared = manifest.get("payload_content_hash")
    if declared is not None and declared != payload_hash:
        raise ValueError("Slack materialized artifact payload hash mismatched")
    pinned = {"label": label, "payload_path": payload_path, "payload_content_hash": payload_hash}
    pinned_bytes = json.dumps(pinned, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
    return f"{label}:{octopus_content_hash(pinned_bytes)}"


def find_admitted_arrival_seq(artifact_id, config):
    artifact_id = require_string(artifact_id, "Slack arrival artifact id")
    try:
        uri = f"file:{urllib.parse.quote(os.path.abspath(LEDGER_INDEX_SQLITE))}?mode=ro"
        connection = sqlite3.connect(uri, uri=True, timeout=0.25)
        try:
            if connection.execute("SELECT value FROM index_meta WHERE key = 'schema_version'").fetchone() != ("4",):
                raise RuntimeError("Slack ledger index has unsupported schema")
            row = connection.execute("SELECT seq FROM movements WHERE kind = 'InterfaceArrival' AND from_kind = 'interface' AND from_id = ? AND artifact_id = ? ORDER BY seq ASC LIMIT 1", (INTERFACE_ID, artifact_id)).fetchone()
        finally:
            connection.close()
        if row is not None:
            return require_int(row[0], "Slack admitted arrival seq", 0)
    except (OSError, RuntimeError, ValueError, sqlite3.DatabaseError):
        pass
    try:
        handle = open(MOVEMENTS_JSONL, "rb")
    except FileNotFoundError:
        return None
    maximum = config["ledger"]["max_row_bytes"]
    with handle:
        while True:
            line = handle.readline(maximum + 2)
            if not line:
                break
            if len(line) > maximum + 1:
                raise RuntimeError("Slack movement lookup row exceeds configured ceiling")
            if not line.endswith(b"\n"):
                break
            movement = require_object(json.loads(line), "Slack movement lookup row")
            if movement.get("kind") != "InterfaceArrival" or movement.get("from") != {"Interface": INTERFACE_ID}:
                continue
            artifact = movement.get("artifact")
            if isinstance(artifact, dict) and artifact.get("id") == artifact_id:
                return require_int(movement.get("seq"), "Slack admitted arrival seq", 0)
    return None


def hand_idempotent(manifest_path, config):
    artifact_id = materialized_artifact_id(manifest_path, config)
    evidence = [{"interface_id": INTERFACE_ID, "artifact_id": artifact_id, "label": "slack.arrival.idempotency", "bytes": list(hashlib.sha256(artifact_id.encode("utf-8")).digest())}]
    try:
        return hand_artifact(manifest_path, evidence)
    except ArtifactDeliveryRefused as error:
        refused = error.verdict.get("refused") if isinstance(error.verdict, dict) else None
        details = refused.get("detail") if isinstance(refused, dict) else None
        if isinstance(details, dict) and details.get("artifact_id") == artifact_id and details.get("message") == "arrival evidence already exists in the movement ledger":
            admitted = find_admitted_arrival_seq(artifact_id, config)
            if admitted is not None:
                return admitted
        raise


def hand_json_idempotent(label, value, key, config):
    return hand_idempotent(materialize_artifact(label, encode(value, config), key), config)


def answered(key):
    return os.path.exists(artifact_paths(key)[0])


def hand_answer(label, value, key, config):
    return hand_idempotent(materialize_artifact(label, None if value is None else encode(value, config), key), config)


def request_path(key):
    artifact_paths(key)
    return os.path.join(REQUEST_DIR, f"{key}.json")


def load_request(key):
    try:
        with open_private_read(request_path(key)) as handle:
            value = json.loads(handle.read())
    except FileNotFoundError:
        return None
    if not isinstance(value, dict) or value.get("schema_version") != 1:
        raise ValueError(f"Slack request journal {key} is malformed")
    return value


def record_request(key, value):
    atomic_json(request_path(key), {"schema_version": 1, **sanitize_external_error(value, _CONFIG)}, indent=2)


def forget_request(key):
    try:
        os.unlink(request_path(key))
    except FileNotFoundError:
        pass


def load_state():
    try:
        with open(STATE_PATH, "r", encoding="utf-8") as handle:
            value = json.load(handle)
    except FileNotFoundError:
        return {"schema_version": 1, "bot_user_ids": {}, "mentioned_threads": [], "bot_message_threads": [], "processed": []}
    value = require_keys(value, {"schema_version", "bot_user_ids", "mentioned_threads", "bot_message_threads", "processed"}, set(), "Slack state")
    if value["schema_version"] != 1:
        raise ValueError("Slack state schema_version must be 1")
    if not isinstance(value["bot_user_ids"], dict):
        raise ValueError("Slack state bot_user_ids must be an object")
    for team_id, user_id in value["bot_user_ids"].items():
        if not TEAM_RE.fullmatch(team_id) or not isinstance(user_id, str) or not USER_RE.fullmatch(user_id):
            raise ValueError("Slack state bot_user_ids contains malformed identity")
    for key in ("mentioned_threads", "bot_message_threads", "processed"):
        if not isinstance(value[key], list) or any(not isinstance(item, str) for item in value[key]):
            raise ValueError(f"Slack state {key} must be a string array")
    return value


def update_state(config, mutator):
    with _STATE_LOCK:
        state = load_state()
        mutator(state)
        retention = config["intake"]["dedup_retention"]
        state["mentioned_threads"] = state["mentioned_threads"][-retention:]
        state["bot_message_threads"] = state["bot_message_threads"][-retention:]
        state["processed"] = state["processed"][-retention:]
        atomic_json(STATE_PATH, state, indent=2)
        return state


def event_user(envelope):
    payload = envelope.get("payload") if isinstance(envelope.get("payload"), dict) else {}
    if envelope.get("type") == "events_api":
        event = payload.get("event") if isinstance(payload.get("event"), dict) else {}
        assistant_thread = event.get("assistant_thread") if isinstance(event.get("assistant_thread"), dict) else {}
        if event.get("subtype") in {"message_changed", "message_deleted"}:
            message = event.get("message") if isinstance(event.get("message"), dict) else {}
            previous = event.get("previous_message") if isinstance(event.get("previous_message"), dict) else {}
            return message.get("user") or previous.get("user") or event.get("user")
        user = event.get("user") or event.get("user_id") or assistant_thread.get("user_id")
        if isinstance(user, dict):
            return user.get("id")
        return user
    if envelope.get("type") == "slash_commands":
        return payload.get("user_id")
    if envelope.get("type") == "interactive":
        user = payload.get("user") if isinstance(payload.get("user"), dict) else {}
        return user.get("id")
    return None


def configured_ids(config, list_key, env_name, pattern):
    return set(config["authz"][list_key]) | env_ids(env_name, pattern)


def allowed_users(config):
    return configured_ids(config, "allowed_user_ids", config["environment"]["allowed_user_ids_env"], USER_RE)


def authorized(config, envelope):
    team, channel, _, _ = event_coordinates(envelope)
    user = event_user(envelope)
    teams = configured_ids(config, "allowed_team_ids", config["authz"]["allowed_team_ids_env"], TEAM_RE)
    channels = configured_ids(config, "allowed_channel_ids", config["environment"]["allowed_channel_ids_env"], CHANNEL_RE)
    users = allowed_users(config)
    allow_all_override = env_flag(config["authz"]["allow_all_users_env"])
    allow_all = config["authz"]["allow_all_users"] if allow_all_override is None else allow_all_override
    if teams and team not in teams:
        return False, "team_not_allowed"
    if channels and channel is not None and channel not in channels:
        return False, "channel_not_allowed"
    if user is None:
        if config["authz"]["deny_unmatched"]:
            return False, "user_missing"
        return True, "allowed_without_user"
    if allow_all or user in users:
        return True, "allowed_user"
    return False, "user_not_allowed"


def event_body(envelope):
    payload = envelope.get("payload") if isinstance(envelope.get("payload"), dict) else {}
    if envelope.get("type") == "events_api":
        return payload.get("event") if isinstance(payload.get("event"), dict) else {}
    return payload


def message_shape(envelope):
    if envelope.get("type") != "events_api":
        return None
    event = event_body(envelope)
    if event.get("type") not in {"message", "app_mention"}:
        return None
    subtype = event.get("subtype")
    if subtype == "message_changed":
        return event.get("message") if isinstance(event.get("message"), dict) else event
    if subtype == "message_deleted":
        return event.get("previous_message") if isinstance(event.get("previous_message"), dict) else event
    return event


def should_admit(config, envelope, state):
    allowed, reason = authorized(config, envelope)
    if not allowed:
        return False, reason
    if envelope.get("type") == "events_api":
        event = event_body(envelope)
        if event.get("type") not in config["intake"]["event_types"]:
            return False, "event_type_not_declared"
    elif envelope.get("type") == "interactive":
        payload = envelope.get("payload") if isinstance(envelope.get("payload"), dict) else {}
        if payload.get("type") not in config["intake"]["interactive_types"]:
            return False, "interactive_type_not_declared"
    elif envelope.get("type") != "slash_commands":
        return False, "envelope_type_not_declared"
    message = message_shape(envelope)
    if message is None:
        return True, "authorized_event"
    subtype = event_body(envelope).get("subtype")
    if subtype == "message_changed" and not config["gating"]["accept_message_edits"]:
        return False, "message_edits_disabled"
    if subtype == "message_deleted" and not config["gating"]["accept_message_deletes"]:
        return False, "message_deletes_disabled"
    if (message.get("bot_id") or subtype == "bot_message") and not config["gating"]["accept_bot_events"]:
        return False, "bot_event"
    team, channel, thread, ts = event_coordinates(envelope)
    if isinstance(channel, str) and channel.startswith("D"):
        return (config["gating"]["respond_in_dms"], "direct_message")
    free = set(config["gating"]["free_response_channel_ids"]) | env_ids(config["gating"]["free_response_channel_ids_env"], CHANNEL_RE)
    if channel in free:
        return True, "free_response_channel"
    if not config["gating"]["respond_in_channels_on_mention_only"]:
        return True, "channel_response_enabled"
    bot = state.get("bot_user_ids", {}).get(team)
    text = str(message.get("text") or "")
    blocks_text = rich_text(message.get("blocks"))
    explicitly_mentioned = bool(bot and (f"<@{bot}>" in text or f"<@{bot}>" in blocks_text))
    if envelope.get("type") == "events_api" and event_body(envelope).get("type") == "app_mention":
        explicitly_mentioned = True
    if any(re.search(pattern, text, re.IGNORECASE) for pattern in config["gating"]["mention_patterns"]):
        explicitly_mentioned = True
    thread_key = f"{channel}:{thread or ts}" if channel and (thread or ts) else None
    if explicitly_mentioned:
        if config["gating"]["thread_sticky_after_mention"] and thread_key:
            def add_mentioned(current):
                if thread_key not in current["mentioned_threads"]:
                    current["mentioned_threads"].append(thread_key)
            update_state(config, add_mentioned)
        return True, "mentioned"
    if thread_key and thread_key in state["mentioned_threads"]:
        return True, "mentioned_thread"
    if thread_key and thread_key in state["bot_message_threads"]:
        return True, "bot_reply_thread"
    return False, "mention_required"


def callback_handle(payload, event_key):
    response_url = payload.get("response_url")
    trigger_id = payload.get("trigger_id")
    if not isinstance(response_url, str) and not isinstance(trigger_id, str):
        return None
    seed = require_string(event_key, "Slack callback event key") + "\0" + (response_url or "") + "\0" + (trigger_id or "")
    handle = hashlib.sha256(seed.encode("utf-8")).hexdigest()
    atomic_json(os.path.join(CALLBACK_DIR, f"{handle}.json"), {"schema_version": 1, "response_url": response_url, "trigger_id": trigger_id, "created_at_unix_ms": int(time.time() * 1000)})
    return handle


def raw_event_path(key):
    safe = re.sub(r"[^A-Za-z0-9_.-]", "_", key)[:180]
    return os.path.join(RAW_DIR, f"{safe}.json")


def pending_event_path(key):
    safe = re.sub(r"[^A-Za-z0-9_.-]", "_", key)[:180]
    return os.path.join(PENDING_DIR, f"{safe}.json")


def persist_envelope(config, envelope):
    key = envelope_key(envelope)
    pending = pending_event_path(key)
    try:
        with open(pending, "r", encoding="utf-8") as handle:
            current = json.load(handle)
        require_object(current, "Slack pending event")
        return key, current
    except FileNotFoundError:
        pass
    if len(os.listdir(PENDING_DIR)) >= config["intake"]["max_pending_events"]:
        raise RuntimeError("Slack pending event ceiling reached")
    raw = json.dumps(envelope, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode("utf-8")
    raw_path = raw_event_path(key)
    atomic_bytes(raw_path, raw)
    record = {
        "schema_version": 1,
        "key": key,
        "raw_path": raw_path,
        "raw_sha256": hashlib.sha256(raw).hexdigest(),
        "status": "pending",
        "attempts": 0,
        "created_at_unix_ms": int(time.time() * 1000),
    }
    atomic_json(pending, record, indent=2)
    return key, record


TEXT_SUFFIXES = {".txt", ".md", ".json", ".csv", ".log", ".py", ".rs", ".js", ".ts", ".yaml", ".yml", ".toml"}
IMAGE_MIMETYPES = {"image/png", "image/jpeg", "image/webp"}


def media_rows(api, config, event, key, images, team_id=None, download=True):
    files = event.get("files") if isinstance(event.get("files"), list) else []
    rows = []
    for index, file_row in enumerate(files[: config["media"]["max_files_per_event"]]):
        if not isinstance(file_row, dict):
            continue
        row = {
            "id": file_row.get("id"),
            "name": file_row.get("name"),
            "title": file_row.get("title"),
            "mimetype": file_row.get("mimetype"),
            "filetype": file_row.get("filetype"),
            "size": file_row.get("size"),
            "mode": file_row.get("mode"),
            "subtype": file_row.get("subtype"),
            "permalink": file_row.get("permalink"),
        }
        url = file_row.get("url_private_download") or file_row.get("url_private")
        filename = str(file_row.get("name") or file_row.get("id") or f"file-{index}")
        suffix = os.path.splitext(filename)[1][:20].lower()
        size = file_row.get("size")
        textual = str(file_row.get("mimetype") or "").startswith("text/") or suffix in TEXT_SUFFIXES
        small = isinstance(size, int) and not isinstance(size, bool) and 0 <= size <= config["media"]["text_preview_bytes"]
        downloadable = download and config["media"]["download_enabled"] and isinstance(url, str) and url
        if downloadable and textual and small:
            try:
                content, _ = api.download(url, config["media"]["allowed_download_hosts"], config["media"]["text_preview_bytes"], team_id=team_id)
                row["text_preview"] = content.decode("utf-8", "replace")
            except Exception as error:
                row["download_error"] = sanitize_external_error(error, config)
        elif downloadable and file_row.get("mimetype") in IMAGE_MIMETYPES and isinstance(size, int) and 0 < size <= min(config["media"]["max_download_bytes"], image_room(images, config["media"])):
            try:
                content, _ = api.download(url, config["media"]["allowed_download_hosts"], size, team_id=team_id)
                directory = ensure_private_directory(os.path.join(MEDIA_DIR, re.sub(r"[^A-Za-z0-9_.-]", "_", key)[:180]))
                path = os.path.join(directory, f"{index + 1:02d}-{re.sub(r'[^A-Za-z0-9._-]', '_', filename).strip('._')[:120] or 'image'}")
                atomic_bytes(path, content)
                row["sha256"] = hashlib.sha256(content).hexdigest()
                embed_image(images, row, path, content, config["media"])
            except Exception as error:
                row["download_error"] = sanitize_external_error(error, config)
        rows.append(row)
    return rows


def prune_media(config):
    cutoff = time.time() - config["media"]["retention_days"] * 86400
    for name in os.listdir(MEDIA_DIR):
        path = os.path.join(MEDIA_DIR, name)
        status = os.lstat(path)
        if stat.S_ISDIR(status.st_mode) and status.st_mtime < cutoff:
            shutil.rmtree(path)


def normalized_event(api, config, envelope, record):
    payload = envelope.get("payload") if isinstance(envelope.get("payload"), dict) else {}
    event = event_body(envelope)
    team, channel, thread, ts = event_coordinates(envelope)
    message = message_shape(envelope) or {}
    if event.get("type") in {"file_shared", "file_created", "file_change"} and not message.get("files"):
        file_id = event.get("file_id") or event.get("file")
        if isinstance(file_id, str) and file_id:
            try:
                file_info = api.call("files.info", {"file": file_id}, safe_retry=True, team_id=team)
                if isinstance(file_info.get("file"), dict):
                    message = {**message, "files": [file_info["file"]]}
            except Exception as error:
                message = {**message, "file_lookup_error": sanitize_external_error(error, config)}
    handle = callback_handle(payload, record["key"]) if envelope.get("type") in {"interactive", "slash_commands"} else None
    normalized_payload = json.loads(json.dumps(payload))
    if isinstance(normalized_payload, dict):
        normalized_payload.pop("response_url", None)
        normalized_payload.pop("trigger_id", None)
        if isinstance(normalized_payload.get("actions"), list):
            for action in normalized_payload["actions"]:
                if isinstance(action, dict):
                    action.pop("url", None)
    text = message.get("text") or payload.get("text") or ""
    blocks = message.get("blocks") if isinstance(message.get("blocks"), list) else []
    thread_context = []
    if channel and thread and config["intake"]["thread_context_max_messages"]:
        try:
            context_response = api.call("conversations.replies", {"channel": channel, "ts": thread, "limit": min(100, config["intake"]["thread_context_max_messages"])}, safe_retry=True, team_id=team)
            messages = context_response.get("messages") if isinstance(context_response.get("messages"), list) else []
            for context_message in messages[-config["intake"]["thread_context_max_messages"] :]:
                if not isinstance(context_message, dict):
                    continue
                context_blocks = context_message.get("blocks") if isinstance(context_message.get("blocks"), list) else []
                thread_context.append({"ts": context_message.get("ts"), "user_id": context_message.get("user"), "bot_id": context_message.get("bot_id"), "text": context_message.get("text") or "", "rich_text": rich_text(context_blocks), "edited": context_message.get("edited")})
        except Exception as error:
            thread_context = [{"context_error": sanitize_external_error(error, config)}]
    images = []
    files = media_rows(api, config, message or event, record["key"], images, team_id=team)
    value = {
        "schema_version": 3,
        "event_key": record["key"],
        "envelope_type": envelope.get("type"),
        "event_type": event.get("type") if isinstance(event, dict) else None,
        "event_subtype": event.get("subtype") if isinstance(event, dict) else None,
        "team_id": team,
        "channel_id": channel,
        "thread_ts": thread,
        "message_ts": ts,
        "user_id": event_user(envelope),
        "message": {
            "text": text,
            "rich_text": rich_text(blocks),
            "files": files,
        },
        "thread_context": thread_context,
        "raw_event_sha256": record["raw_sha256"],
    }
    if images:
        value["media"] = images
    if envelope.get("type") in {"interactive", "slash_commands"}:
        value.update({"callback_handle": handle, "trigger_handle": handle if isinstance(payload.get("trigger_id"), str) else None, "payload": normalized_payload})
    return value


def mark_pending(record, status, **updates):
    record = dict(record)
    record["status"] = status
    record.update(updates)
    atomic_json(pending_event_path(record["key"]), record, indent=2)
    return record


def load_envelope(record):
    with open(record["raw_path"], "rb") as handle:
        raw = handle.read()
    if hashlib.sha256(raw).hexdigest() != record["raw_sha256"]:
        raise RuntimeError("Slack raw event evidence digest mismatched")
    value = json.loads(raw)
    if not isinstance(value, dict):
        raise RuntimeError("Slack raw event evidence is not an object")
    return value


def finish_pending(config, record):
    def mark(current):
        if record["key"] not in current["processed"]:
            current["processed"].append(record["key"])
    update_state(config, mark)
    try:
        os.unlink(pending_event_path(record["key"]))
    except FileNotFoundError:
        pass


def process_pending_event(api, config, record):
    envelope = load_envelope(record)
    if record["key"] in load_state()["processed"]:
        finish_pending(config, record)
        return None
    admitted, reason = should_admit(config, envelope, load_state())
    if not admitted:
        throttled_stderr(config, f"not-admitted:{reason}", f"Slack event {record['key']} not admitted: {reason}")
        finish_pending(config, record)
        return None
    normalized = normalized_event(api, config, envelope, record)
    seq = hand_json_idempotent(config["intake"]["event_label"], normalized, f"slack-event-{record['key']}", config)
    finish_pending(config, record)
    return seq


def retry_pending(api, config):
    prune_media(config)
    for name in sorted(os.listdir(PENDING_DIR)):
        if not name.endswith(".json"):
            continue
        path = os.path.join(PENDING_DIR, name)
        try:
            with open(path, "r", encoding="utf-8") as handle:
                record = json.load(handle)
            if not isinstance(record, dict) or record.get("schema_version") != 1:
                raise ValueError("Slack pending record is malformed")
            process_pending_event(api, config, record)
        except Exception as error:
            stderr(config, f"Slack pending event {name} failed: {error}")


def load_cursor():
    try:
        with open(CURSOR_PATH, "r", encoding="utf-8") as handle:
            value = json.load(handle)
    except FileNotFoundError:
        return 0
    value = require_keys(value, {"schema_version", "next_seq"}, set(), "Slack ledger cursor")
    if value["schema_version"] != 1:
        raise ValueError("Slack ledger cursor schema_version must be 1")
    return require_int(value["next_seq"], "Slack ledger cursor.next_seq", 0)


def commit_cursor(next_seq):
    atomic_json(CURSOR_PATH, {"schema_version": 1, "next_seq": next_seq})


def ledger_end_seq(max_row_bytes):
    try:
        handle = open_regular_file_stable(MOVEMENTS_JSONL, "movement ledger")
    except FileNotFoundError:
        return 0
    next_seq = 0
    with handle:
        while True:
            row_offset = handle.tell()
            line = handle.readline(max_row_bytes + 1)
            if not line or not line.endswith(b"\n"):
                return next_seq
            if len(line) > max_row_bytes:
                raise RuntimeError(f"Slack ledger row at byte {row_offset} exceeds {max_row_bytes} bytes")
            seq = require_int(require_object(json.loads(line), f"Slack ledger movement").get("seq"), f"Slack ledger movement.seq", 0)
            next_seq = max(next_seq, seq + 1)


def start_cursor(max_row_bytes):
    if not os.path.exists(CURSOR_PATH):
        commit_cursor(ledger_end_seq(max_row_bytes))


def content_hexdigest(content):
    if isinstance(content, list):
        if len(content) != 32 or any(isinstance(item, bool) or not isinstance(item, int) or item < 0 or item > 255 for item in content):
            raise ValueError("Slack artifact content list is not a 32-byte hash")
        content = bytes(content)
    if not isinstance(content, bytes) or len(content) != 32:
        raise ValueError("Slack artifact content must be a 32-byte hash")
    return content.hex()


def read_bounded_regular(path, maximum, name):
    descriptor = os.open(path, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0))
    try:
        opened = os.fstat(descriptor)
        linked = os.lstat(path)
        if not stat.S_ISREG(opened.st_mode) or stat.S_ISLNK(linked.st_mode) or not stat.S_ISREG(linked.st_mode):
            raise ValueError(f"Slack {name} is not a regular file")
        if (opened.st_dev, opened.st_ino) != (linked.st_dev, linked.st_ino):
            raise ValueError(f"Slack {name} changed while opening")
        if opened.st_size > maximum:
            raise ValueError(f"Slack {name} exceeds {maximum} bytes")
        with os.fdopen(descriptor, "rb") as handle:
            descriptor = -1
            value = handle.read(maximum + 1)
        if len(value) > maximum:
            raise ValueError(f"Slack {name} grew above {maximum} bytes while reading")
        return value
    finally:
        if descriptor >= 0:
            os.close(descriptor)


def open_regular_file_stable(path, name):
    descriptor = os.open(path, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0))
    try:
        opened = os.fstat(descriptor)
        linked = os.lstat(path)
        if not stat.S_ISREG(opened.st_mode) or stat.S_ISLNK(linked.st_mode) or not stat.S_ISREG(linked.st_mode) or (opened.st_dev, opened.st_ino) != (linked.st_dev, linked.st_ino):
            raise RuntimeError(f"Slack {name} changed while opening")
        return os.fdopen(descriptor, "rb")
    except Exception:
        os.close(descriptor)
        raise


def materialize_movement_payload(content, config):
    digest = content_hexdigest(content)
    manifest_bytes = read_bounded_regular(
        os.path.join(BLOBS_DIR, digest),
        config["outbound"]["max_manifest_bytes"],
        "cached artifact manifest",
    )
    if octopus_content_hash(manifest_bytes) != digest:
        raise ValueError("Slack cached artifact manifest failed content-address verification")
    manifest = json.loads(manifest_bytes)
    manifest = require_object(manifest, "Slack cached artifact manifest")
    label = require_string(manifest.get("label"), "Slack cached artifact label")
    payload_digest = manifest.get("payload_content_hash")
    if not isinstance(payload_digest, str) or not re.fullmatch(r"[0-9a-f]{64}", payload_digest):
        raise ValueError("Slack cached artifact manifest has no payload hash")
    payload = read_bounded_regular(
        os.path.join(BLOBS_DIR, payload_digest),
        config["outbound"]["max_payload_bytes"],
        "cached artifact payload",
    )
    if octopus_content_hash(payload) != payload_digest:
        raise ValueError("Slack cached artifact payload failed content-address verification")
    return label, payload, digest


def movement_interface_endpoint(value):
    if not isinstance(value, dict) or set(value) != {"Interface"}:
        return None
    identifier = value.get("Interface")
    if not isinstance(identifier, str) or not identifier or identifier.strip() != identifier:
        return None
    return identifier


def require_outbound_runtime(movement):
    metadata = require_object(
        movement.get("invocation_metadata"),
        "Slack outbound movement.invocation_metadata",
    )
    runtime = require_object(
        metadata.get("runtime"),
        "Slack outbound movement runtime metadata",
    )
    for field in ("run", "ee", "worker", "checkpoint", "interface"):
        require_string(
            runtime.get(field),
            f"Slack outbound movement runtime.{field}",
        )
    require_int(runtime.get("epoch"), "Slack outbound movement runtime.epoch", 0)
    if runtime["interface"] != INTERFACE_ID:
        raise ValueError("Slack outbound runtime interface does not match its exact target")
    return runtime


def read_index(next_seq, limit):
    uri = f"file:{urllib.parse.quote(os.path.abspath(LEDGER_INDEX_SQLITE))}?mode=ro"
    connection = sqlite3.connect(uri, uri=True, timeout=0.25)
    try:
        schema = connection.execute("SELECT value FROM index_meta WHERE key = 'schema_version'").fetchone()
        if schema != ("4",):
            raise RuntimeError("Slack ledger index schema is unsupported")
        rows = connection.execute(
            "SELECT seq, artifact_content, runtime_run, runtime_checkpoint, from_id FROM movements "
            "WHERE runtime_interface = ? AND kind = 'InterfaceOutbound' "
            "AND from_kind = 'interface' AND to_kind = 'interface' AND to_id = ? AND seq >= ? "
            "ORDER BY seq ASC LIMIT ?",
            (INTERFACE_ID, INTERFACE_ID, next_seq, limit),
        ).fetchall()
    finally:
        connection.close()
    result = []
    for seq, content, run, checkpoint, source_interface in rows:
        seq = require_int(seq, "Slack ledger index seq", 0)
        require_string(source_interface, f"Slack ledger index movement {seq}.from_id")
        if content is not None and (not isinstance(content, str) or not re.fullmatch(r"[0-9a-f]{64}", content)):
            raise RuntimeError(f"Slack ledger index movement {seq} has malformed artifact hash")
        require_string(run, f"Slack ledger index movement {seq}.run")
        require_string(checkpoint, f"Slack ledger index movement {seq}.checkpoint")
        result.append((seq, bytes.fromhex(content) if content else None, run))
    return result


def read_ledger(next_seq, limit, max_row_bytes):
    global _LEDGER_FALLBACK_IDENTITY, _LEDGER_FALLBACK_OFFSET, _LEDGER_FALLBACK_SIZE
    max_row_bytes = require_int(max_row_bytes, "Slack ledger.max_row_bytes", 256, 16777216)
    with _LEDGER_LOCK:
        try:
            handle = open_regular_file_stable(MOVEMENTS_JSONL, "movement ledger")
        except FileNotFoundError:
            return []
        result = []
        with handle:
            status = os.fstat(handle.fileno())
            size = status.st_size
            identity = (status.st_dev, status.st_ino)
            if identity != _LEDGER_FALLBACK_IDENTITY or size < _LEDGER_FALLBACK_SIZE or _LEDGER_FALLBACK_OFFSET > size:
                _LEDGER_FALLBACK_OFFSET = 0
            handle.seek(_LEDGER_FALLBACK_OFFSET)
            safe_offset = _LEDGER_FALLBACK_OFFSET
            has_uncommitted = False
            while True:
                row_offset = handle.tell()
                line = handle.readline(max_row_bytes + 1)
                if not line:
                    if not has_uncommitted:
                        safe_offset = handle.tell()
                    break
                if len(line) > max_row_bytes:
                    raise RuntimeError(f"Slack ledger row at byte {row_offset} exceeds {max_row_bytes} bytes")
                if not line.endswith(b"\n"):
                    raise RuntimeError(f"Slack ledger row at byte {row_offset} is partial")
                movement = require_object(json.loads(line), "Slack ledger movement")
                seq = require_int(movement.get("seq"), "Slack ledger movement.seq", 0)
                if seq < next_seq:
                    if not has_uncommitted:
                        safe_offset = handle.tell()
                    continue
                if movement.get("kind") != "InterfaceOutbound":
                    if not has_uncommitted:
                        safe_offset = handle.tell()
                    continue
                if movement_interface_endpoint(movement.get("to")) != INTERFACE_ID:
                    if not has_uncommitted:
                        safe_offset = handle.tell()
                    continue
                if movement_interface_endpoint(movement.get("from")) is None:
                    if not has_uncommitted:
                        safe_offset = handle.tell()
                    continue
                runtime = require_outbound_runtime(movement)
                artifact = movement.get("artifact") if isinstance(movement.get("artifact"), dict) else None
                if not has_uncommitted:
                    safe_offset = row_offset
                    has_uncommitted = True
                result.append((seq, artifact.get("content") if artifact else None, runtime.get("run")))
                if len(result) >= limit:
                    break
            _LEDGER_FALLBACK_OFFSET = safe_offset
            _LEDGER_FALLBACK_SIZE = os.fstat(handle.fileno()).st_size
            _LEDGER_FALLBACK_IDENTITY = identity
        return result


def read_outbound(next_seq, limit=128, config=None):
    global _LEDGER_INDEX_WARNING
    config = _CONFIG if config is None else config
    if config is None:
        config = load_config()
    try:
        return read_index(next_seq, limit)
    except (OSError, RuntimeError, ValueError, sqlite3.DatabaseError) as error:
        message = sanitize_external_error(error, config)
        if message != _LEDGER_INDEX_WARNING:
            stderr(config, f"Slack ledger index unavailable; reading ledger truth: {message}")
            _LEDGER_INDEX_WARNING = message
        return read_ledger(next_seq, limit, config["ledger"]["max_row_bytes"])


def require_channel(value, name):
    value = require_string(value, name)
    if not CHANNEL_RE.fullmatch(value):
        raise ValueError(f"{name} is not a Slack channel id")
    return value


def require_ts(value, name, optional=False):
    if value is None and optional:
        return None
    value = require_string(value, name)
    if not TS_RE.fullmatch(value):
        raise ValueError(f"{name} is not a Slack timestamp")
    return value


def default_conversation(config):
    home = os.environ.get(config["environment"]["home_channel_env"], "").strip()
    if home:
        return home
    users = allowed_users(config)
    if len(users) == 1:
        return next(iter(users))
    raise ValueError("Slack operation names no channel, and no home channel or single allowed member is configured")


def target_channel(args, config, direct_message=False):
    channel = args.get("channel") or args.get("channel_id")
    if channel is None and direct_message:
        channel = default_conversation(config)
    channel = require_string(channel, "Slack operation channel")
    if direct_message and USER_RE.fullmatch(channel):
        return channel
    return require_channel(channel, "Slack operation channel")


def target_thread(args):
    return require_ts(args.get("thread_ts"), "Slack operation thread_ts", optional=True)


def target_team(args):
    team = args.get("team_id")
    if team is not None and (not isinstance(team, str) or not TEAM_RE.fullmatch(team)):
        raise ValueError("Slack operation team_id is malformed")
    return team


def split_text(text, maximum):
    text = require_string(text, "Slack message text")
    if len(text) <= maximum:
        return [text]
    chunks = []
    remaining = text
    while remaining:
        if len(remaining) <= maximum:
            chunks.append(remaining)
            break
        cut = remaining.rfind("\n", 0, maximum + 1)
        if cut < maximum // 2:
            cut = remaining.rfind(" ", 0, maximum + 1)
        if cut < maximum // 2:
            cut = maximum
        chunks.append(remaining[:cut].rstrip())
        remaining = remaining[cut:].lstrip()
    return chunks


def idempotency_uuid(seed):
    digest = hashlib.sha256(seed.encode("utf-8")).hexdigest()
    return f"{digest[:8]}-{digest[8:12]}-4{digest[13:16]}-a{digest[17:20]}-{digest[20:32]}"


def read_callback_record(handle):
    handle = require_string(handle, "Slack callback handle")
    if not re.fullmatch(r"[0-9a-f]{64}", handle):
        raise ValueError("Slack callback handle is malformed")
    path = os.path.join(CALLBACK_DIR, f"{handle}.json")
    with open_private_read(path) as source:
        value = require_keys(json.loads(source.read()), {"schema_version", "response_url", "trigger_id", "created_at_unix_ms"}, set(), "Slack callback record")
    if value["schema_version"] != 1:
        raise ValueError("Slack callback record schema is unsupported")
    return value


def callback_url(handle):
    return require_string(read_callback_record(handle)["response_url"], "Slack callback response_url")


def callback_trigger(handle):
    handle = require_string(handle, "Slack trigger handle")
    if not re.fullmatch(r"[0-9a-f]{64}", handle):
        raise ValueError("Slack trigger handle is malformed")
    return require_string(read_callback_record(handle)["trigger_id"], "Slack callback trigger_id")


def artifact_blob(blob_id, maximum):
    if not isinstance(blob_id, str) or not re.fullmatch(r"[0-9a-f]{64}", blob_id):
        raise ValueError("Slack file upload blob_id must be a content hash")
    content = read_bounded_regular(os.path.join(BLOBS_DIR, blob_id), maximum, "file upload blob")
    if octopus_content_hash(content) != blob_id:
        raise ValueError("Slack file upload blob failed content-address verification")
    return content


def record_bot_thread(config, response, channel, thread):
    if not config["gating"]["thread_sticky_after_bot_reply"]:
        return
    ts = response.get("ts") if isinstance(response, dict) else None
    effective = thread or ts
    channel = (response.get("channel") if isinstance(response, dict) else None) or channel
    if not effective or not channel:
        return
    key = f"{channel}:{effective}"
    def add(current):
        if key not in current["bot_message_threads"]:
            current["bot_message_threads"].append(key)
    update_state(config, add)


class PartialDelivery(Exception):
    def __init__(self, error, posted):
        super().__init__(str(error))
        self.error = error
        self.posted = posted


def post_message(api, config, args, source_hash, team):
    outbound = config["outbound"]
    channel = target_channel(args, config, direct_message=True)
    thread = target_thread(args)
    text = args.get("text")
    blocks = safe_blocks(args.get("blocks"), outbound["max_block_json_bytes"])
    if len(blocks) > outbound["max_blocks"]:
        raise ValueError("Slack message blocks exceed configured count")
    if not isinstance(text, str) or not text:
        if not blocks:
            raise ValueError("Slack post_message needs text, blocks or both")
        text = args.get("fallback_text") or "Message"
    chunks = split_text(text, outbound["max_message_chars"])
    if blocks and len(chunks) != 1:
        raise ValueError("Slack block message text cannot require splitting")
    results = []
    for index, chunk in enumerate(chunks):
        params = {"channel": channel, "text": slack_mrkdwn(chunk), "mrkdwn": args.get("mrkdwn", True)}
        if thread:
            params["thread_ts"] = thread
        if args.get("reply_broadcast") is not None:
            params["reply_broadcast"] = bool(args["reply_broadcast"])
        if blocks and index == 0:
            params["blocks"] = blocks
        if isinstance(args.get("metadata"), dict):
            params["metadata"] = args["metadata"]
        params["client_msg_id"] = idempotency_uuid(f"{source_hash}:{index}")
        try:
            response = api.call("chat.postMessage", params, team_id=team)
        except Exception as error:
            if results:
                raise PartialDelivery(error, [item.get("ts") for item in results]) from None
            raise
        record_bot_thread(config, response, channel, thread)
        results.append(response)
    status_clear = None
    if thread:
        try:
            status_clear = {"ok": True, "response": api.call("assistant.threads.setStatus", {"channel_id": channel, "thread_ts": thread, "status": ""}, team_id=team)}
        except Exception as error:
            status_clear = {"ok": False, "error": sanitize_external_error(error, config)}
    return {"messages": results, "status_clear": status_clear}


def message_files(api, config, response, key, media, team):
    for message in response.get("messages") if isinstance(response.get("messages"), list) else []:
        if isinstance(message, dict) and isinstance(message.get("files"), list):
            message["files"] = media_rows(api, config, message, key, media, team_id=team)


def execute_operation(api, config, handler, args, source_hash, key, media):
    outbound = config["outbound"]
    operation = handler["operation"]
    team = target_team(args)
    if operation == "message.post":
        return post_message(api, config, args, source_hash, team)
    if operation == "message.ephemeral":
        channel = target_channel(args, config)
        params = {"channel": channel, "user": args["user"], "text": slack_mrkdwn(require_string(args["text"], "Slack ephemeral text", maximum=outbound["max_message_chars"]))}
        blocks = safe_blocks(args.get("blocks"), outbound["max_block_json_bytes"])
        if blocks:
            params["blocks"] = blocks
        thread = target_thread(args)
        if thread:
            params["thread_ts"] = thread
        return {"response": api.call("chat.postEphemeral", params, team_id=team)}
    if operation == "message.update":
        params = {"channel": target_channel(args, config), "ts": require_ts(args.get("ts"), "Slack update_message ts")}
        if "text" in args:
            params["text"] = slack_mrkdwn(require_string(args["text"], "Slack update_message text", maximum=outbound["max_message_chars"]))
        blocks = safe_blocks(args.get("blocks"), outbound["max_block_json_bytes"])
        if blocks:
            params["blocks"] = blocks
        if "text" not in params and "blocks" not in params:
            raise ValueError("Slack update_message needs text, blocks or both")
        return {"response": api.call("chat.update", params, team_id=team)}
    if operation == "message.delete":
        return {"response": api.call("chat.delete", {"channel": target_channel(args, config), "ts": require_ts(args.get("ts"), "Slack delete_message ts")}, team_id=team)}
    if operation in {"reaction.add", "reaction.remove"}:
        method = "reactions.add" if operation == "reaction.add" else "reactions.remove"
        params = {"channel": target_channel(args, config), "timestamp": require_ts(args.get("timestamp"), "Slack reaction timestamp"), "name": args["name"]}
        return {"response": api.call(method, params, team_id=team)}
    if operation == "file.upload":
        channel = target_channel(args, config)
        files = [{"filename": item["filename"], "content": artifact_blob(item["blob_id"], config["media"]["max_upload_bytes"]), "title": item.get("title")} for item in args["files"]]
        initial_comment = args.get("initial_comment")
        if initial_comment is not None:
            initial_comment = slack_mrkdwn(require_string(initial_comment, "Slack upload_files initial_comment", maximum=outbound["max_message_chars"]))
        return {"response": api.upload_multiple_external(channel, files, initial_comment=initial_comment, thread_ts=target_thread(args), team_id=team)}
    if operation in {"view.open", "view.push"}:
        if ("trigger_handle" in args) == ("trigger_id" in args):
            raise ValueError("Slack views need exactly one of trigger_handle or trigger_id")
        trigger_id = callback_trigger(args["trigger_handle"]) if "trigger_handle" in args else args["trigger_id"]
        method = "views.open" if operation == "view.open" else "views.push"
        return {"response": api.call(method, {"trigger_id": trigger_id, "view": args["view"]}, team_id=team)}
    if operation == "view.update":
        if ("view_id" in args) == ("external_id" in args):
            raise ValueError("Slack update_view needs exactly one of view_id or external_id")
        params = {key: args[key] for key in ("view", "view_id", "external_id", "hash") if key in args}
        return {"response": api.call("views.update", params, team_id=team)}
    if operation == "view.publish":
        return {"response": api.call("views.publish", {"user_id": args["user_id"], "view": args["view"]}, team_id=team)}
    if operation == "callback.respond":
        return {"response": api.post_response_url(callback_url(args["handle"]), args["body"])}
    if operation == "thread.status":
        params = {"channel_id": target_channel(args, config), "thread_ts": require_ts(args.get("thread_ts"), "Slack set_thread_status thread_ts"), "status": args["status"]}
        return {"response": api.call("assistant.threads.setStatus", params, team_id=team)}
    if operation == "profile.status.set":
        profile = {"status_text": args.get("status_text", ""), "status_emoji": args.get("status_emoji", ""), "status_expiration": args.get("status_expiration", 0)}
        return {"response": api.call("users.profile.set", {"profile": profile}, team_id=team)}
    if operation == "web_api":
        params = {name: value for name, value in args.items() if name != "team_id"}
        params.update(handler.get("params", {}))
        response = api.call(handler["method"], params, safe_retry=handler["read_only"], team_id=team)
        if handler.get("files"):
            message_files(api, config, response, key, media, team)
        return {"method": handler["method"], "response": response}
    raise ValueError(f"Slack handler {operation!r} is unsupported")


DEFINITE_REFUSALS = {"slack_error", "boundary_rejected", "rate_limited", "proxy_refused"}
INDETERMINATE = "The request stopped before Slack's answer was known, so it may or may not have taken effect. It was not repeated; check the conversation before trying again."


def failure(config, error, read_only):
    wire = error.error if isinstance(error, PartialDelivery) else error
    details = {"category": wire.category, "message": str(wire), **wire.details} if isinstance(wire, SlackWireError) else {"message": str(wire)}
    definite = isinstance(wire, SlackWireError) and wire.category in DEFINITE_REFUSALS
    if not read_only and not definite:
        code, message = "indeterminate_external_effect", INDETERMINATE
    elif isinstance(error, PartialDelivery):
        code, message = "partial_delivery", "Slack posted only the first parts of this message; posted_ts lists them."
    elif definite:
        code, message = "slack_refused", "Slack refused the request, so nothing was changed."
    else:
        code, message = "slack_unreachable", "Slack could not be reached, so nothing was read."
    result = {"code": code, "message": message, "slack": sanitize_external_error(details, config)}
    if isinstance(error, PartialDelivery):
        result["posted_ts"] = error.posted
    return result


def process_request(api, config, source_seq, content, run):
    if content is None or run is None:
        throttled_stderr(config, f"request:{source_seq}", f"Slack outbound movement {source_seq} carries no artifact or runtime run, so it is not an operation request")
        return
    label, payload, content_hash = materialize_movement_payload(content, config)
    catalog = load_operations()
    if label != catalog["request_label"]:
        throttled_stderr(config, f"label:{label}", f"Slack answers only {catalog['request_label']} artifacts; outbound {label} is not an operation request")
        return
    key = f"slack-result-{source_seq}-{content_hash[:16]}"
    if answered(key):
        hand_answer(catalog["result_label"], None, key, config)
        forget_request(key)
        return
    try:
        spec, args, correlation_id = parse_request(payload, catalog)
    except RequestError as error:
        hand_answer(catalog["result_label"], answer(catalog["result_label"], error.operation, error.correlation_id, error={"code": error.code, "message": str(error)}), key, config)
        return
    media = []

    def respond(result=None, error=None):
        hand_answer(catalog["result_label"], answer(catalog["result_label"], spec["id"], correlation_id, result=result, error=error, media=media if error is None else None), key, config)
        forget_request(key)

    if load_request(key) is not None:
        respond(error={"code": "indeterminate_external_effect", "message": INDETERMINATE})
        return
    handler = spec["handler_config"]
    read_only = handler.get("read_only") is True
    if not read_only:
        record_request(key, {"source_movement_seq": source_seq, "run": run, "operation": spec["id"], "correlation_id": correlation_id, "started_at_unix_ms": int(time.time() * 1000)})
    try:
        result = execute_operation(api, config, handler, args, content_hash, key, media)
    except ValueError as error:
        respond(error={"code": "rejected", "message": sanitize_external_error(error, config)})
    except Exception as error:
        respond(error=failure(config, error, read_only))
    else:
        respond(result=result)


def outbound_loop(api, config):
    next_seq = load_cursor()
    while True:
        rows = read_outbound(next_seq, config=config)
        for source_seq, content, run in rows:
            try:
                process_request(api, config, source_seq, content, run)
            except Exception as error:
                throttled_stderr(config, f"outbound:{source_seq}", f"Slack outbound movement {source_seq} failed: {error}")
                time.sleep(0.25)
                break
            next_seq = source_seq + 1
            commit_cursor(next_seq)
        time.sleep(0.05)


def authenticate(api, config):
    responses = []
    bot_ids = {}
    for token in api.bot_tokens:
        response = api.call("auth.test", {}, safe_retry=True, token_override=token)
        user_id = response.get("user_id")
        team_id = response.get("team_id")
        if not isinstance(user_id, str) or not USER_RE.fullmatch(user_id):
            raise RuntimeError("Slack auth.test returned no valid bot user id")
        if not isinstance(team_id, str) or not TEAM_RE.fullmatch(team_id):
            raise RuntimeError("Slack auth.test returned no valid team id")
        if team_id in api.team_tokens:
            raise RuntimeError(f"Slack bot token list repeats workspace {team_id}")
        api.team_tokens[team_id] = token
        bot_ids[team_id] = user_id
        responses.append(response)
    def set_bot(state):
        state["bot_user_ids"] = bot_ids
    update_state(config, set_bot)
    return responses


def intake_loop(api, config):
    retry_pending(api, config)
    initial = config["transport"]["reconnect_backoff_ms"]
    cap = config["transport"]["reconnect_backoff_cap_ms"]
    backoff = initial
    while True:
        wire = None
        try:
            wire = WebSocket(api.open_socket_url(), config["transport"]["connect_timeout_ms"], config["transport"]["max_frame_bytes"], api.proxy_url).connect()
            backoff = initial
            while True:
                envelope = wire.receive_json()
                if envelope.get("type") in {"hello", "warning"} and "envelope_id" not in envelope:
                    continue
                if envelope.get("type") == "disconnect" and "envelope_id" not in envelope:
                    raise SlackWireError("transport_error", f"Slack requested reconnect: {envelope.get('reason')}")
                envelope_id = envelope.get("envelope_id")
                if not isinstance(envelope_id, str) or not envelope_id:
                    raise SlackWireError("malformed_response", "Slack Socket Mode envelope has no envelope_id")
                _, record = persist_envelope(config, envelope)
                wire.send_json({"envelope_id": envelope_id})
                try:
                    process_pending_event(api, config, record)
                except Exception as error:
                    attempts = require_int(record.get("attempts", 0), "Slack pending attempts", 0) + 1
                    safe_error = sanitize_external_error(error, config)
                    mark_pending(record, "retryable_failure", attempts=attempts, last_error=safe_error, last_attempt_unix_ms=int(time.time() * 1000))
                    stderr(config, f"Slack intake envelope {envelope_id} retained after error: {safe_error}")
        except Exception as error:
            stderr(config, f"Slack Socket Mode reconnecting after error: {error}")
            if wire is not None:
                wire.close()
            retry_pending(api, config)
            time.sleep(backoff / 1000.0)
            backoff = min(backoff * 2, cap)


def resolve_tokens(config):
    raw_bot = os.environ.get(config["environment"]["bot_token_env"])
    app = os.environ.get(config["environment"]["app_token_env"])
    if not isinstance(raw_bot, str) or not raw_bot.strip():
        raise ValueError("Slack bot token is not configured; set it in the Slack application setup")
    bots = [token.strip() for token in raw_bot.split(",") if token.strip()]
    if any(re.fullmatch(r"xoxb-[0-9A-Za-z-]{15,4096}", token) is None for token in bots) or len(set(bots)) != len(bots):
        raise ValueError("Slack bot token list is malformed or duplicated")
    if not isinstance(app, str) or not app.strip():
        raise ValueError("Slack app-level token is not configured; set it in the Slack application setup")
    if re.fullmatch(r"xapp-[0-9A-Za-z-]{15,4096}", app.strip()) is None:
        raise ValueError("Slack app-level token is malformed")
    if not allowed_users(config) and not config["authz"]["allow_all_users"] and env_flag(config["authz"]["allow_all_users_env"]) is not True:
        raise ValueError("Slack allowed members are not configured; set at least one member ID in the Slack application setup")
    return bots, app.strip()


def main():
    global _CONFIG
    config = load_config()
    _CONFIG = config
    load_operations()
    start_cursor(config["ledger"]["max_row_bytes"])
    bot, app = resolve_tokens(config)
    api = SlackApi(config, bot, app)
    start_runtime_verdict_reader()
    authenticate(api, config)
    threading.Thread(target=outbound_loop, args=(api, config), daemon=True).start()
    signal_ready()
    intake_loop(api, config)


if __name__ == "__main__":
    try:
        main()
    except Exception as error:
        stderr(_CONFIG, f"Slack interface exited: {error}")
        raise SystemExit(1)
