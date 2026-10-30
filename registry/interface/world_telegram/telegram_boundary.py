import hashlib
import json
import os
import re
import sqlite3
import stat
import sys
import threading
import time
import urllib.parse

from octopus_content_hash import octopus_content_hash

from telegram_contract import require_int
from telegram_contract import require_keys
from telegram_contract import require_object
from telegram_contract import require_string


INTERFACE_ID = os.environ["OCTOPUS_INTERFACE_ID"]
INTERFACE_DIR = os.environ["OCTOPUS_INTERFACE_DIR"]
VAR_DIR = os.environ["OCTOPUS_VAR_DIR"]
MOVEMENTS_JSONL = os.environ["OCTOPUS_MOVEMENTS_JSONL"]
BLOBS_DIR = os.environ["OCTOPUS_BLOBS_DIR"]
LEDGER_INDEX_SQLITE = os.environ["OCTOPUS_LEDGER_INDEX_SQLITE"]
PAYLOAD_DIR = os.path.join(VAR_DIR, "payloads")
REQUEST_DIR = os.path.join(VAR_DIR, "operation-requests")
CURSOR_PATH = os.path.join(VAR_DIR, "ledger-cursor.json")

os.umask(0o077)


class ArtifactDeliveryRefused(RuntimeError):
    def __init__(self, verdict):
        self.verdict = verdict
        super().__init__(f"runtime refused Telegram artifact: {verdict!r}")


_STDOUT_LOCK = threading.Lock()
_VERDICT_CONDITION = threading.Condition()
_VERDICTS = {}
_VERDICT_INPUT_ERROR = None
_NEXT_REQUEST_ID = 1
_LEDGER_LOCK = threading.RLock()
_LEDGER_FALLBACK = {"offset": 0, "size": 0, "identity": None}
_STDERR_LOCK = threading.Lock()
_STDERR = {"budget": 196608, "keys": set()}
SANITIZE = [lambda value: value]


def sanitize(value):
    return SANITIZE[0](value)


def stderr(message, key=None):
    line = str(sanitize(message))[:4000] + "\n"
    size = len(line.encode("utf-8", "replace"))
    with _STDERR_LOCK:
        if key is not None:
            if key in _STDERR["keys"]:
                return
            if len(_STDERR["keys"]) >= 4096:
                _STDERR["keys"].clear()
            _STDERR["keys"].add(key)
        if _STDERR["budget"] <= 0:
            return
        if size > _STDERR["budget"] - 128:
            line = "Telegram diagnostics reached their budget; later messages are not written\n"
            _STDERR["budget"] = 0
        else:
            _STDERR["budget"] -= size
        sys.stderr.write(line)
        sys.stderr.flush()


def forget(prefix):
    with _STDERR_LOCK:
        _STDERR["keys"] = {key for key in _STDERR["keys"] if not key.startswith(prefix)}


def ensure_private_directory(path):
    path = os.path.abspath(path)
    os.makedirs(path, mode=0o700, exist_ok=True)
    current = os.lstat(path)
    if stat.S_ISLNK(current.st_mode) or not stat.S_ISDIR(current.st_mode):
        raise PermissionError(f"Telegram private directory is not a real directory: {path}")
    if current.st_uid != os.getuid():
        raise PermissionError(f"Telegram private directory has a different owner: {path}")
    if stat.S_IMODE(current.st_mode) != 0o700:
        os.chmod(path, 0o700)
    return path


def atomic_bytes(path, payload):
    parent = os.path.dirname(path)
    ensure_private_directory(parent)
    temporary = os.path.join(parent, f".{os.path.basename(path)}.{os.getpid()}.{threading.get_ident()}.{time.time_ns()}.tmp")
    descriptor = None
    try:
        descriptor = os.open(temporary, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
        with os.fdopen(descriptor, "wb") as handle:
            descriptor = None
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
        if descriptor is not None:
            os.close(descriptor)
        try:
            os.unlink(temporary)
        except FileNotFoundError:
            pass


def atomic_json(path, value, indent=None):
    atomic_bytes(path, json.dumps(value, ensure_ascii=False, sort_keys=True, indent=indent).encode("utf-8"))


def read_bounded_regular(path, maximum, name):
    descriptor = os.open(path, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0))
    try:
        opened = os.fstat(descriptor)
        linked = os.lstat(path)
        if not stat.S_ISREG(opened.st_mode) or stat.S_ISLNK(linked.st_mode) or not stat.S_ISREG(linked.st_mode):
            raise ValueError(f"Telegram {name} is not a regular file")
        if (opened.st_dev, opened.st_ino) != (linked.st_dev, linked.st_ino):
            raise ValueError(f"Telegram {name} changed while opening")
        if opened.st_size > maximum:
            raise ValueError(f"Telegram {name} exceeds {maximum} bytes")
        with os.fdopen(descriptor, "rb") as handle:
            descriptor = -1
            value = handle.read(maximum + 1)
        if len(value) > maximum:
            raise ValueError(f"Telegram {name} grew above {maximum} bytes while reading")
        return value
    finally:
        if descriptor >= 0:
            os.close(descriptor)


def read_json_file(path, maximum, name):
    try:
        return json.loads(read_bounded_regular(path, maximum, name))
    except FileNotFoundError:
        return None


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
            raise RuntimeError(f"runtime verdict input failed before Telegram request {request_id}: {_VERDICT_INPUT_ERROR}")
        verdict = _VERDICTS.pop(request_id)
    admitted = verdict.get("admitted")
    if isinstance(admitted, dict) and isinstance(admitted.get("seq"), int) and not isinstance(admitted.get("seq"), bool):
        return admitted["seq"]
    raise ArtifactDeliveryRefused(verdict)


def artifact_paths(key):
    key = require_string(key, "Telegram artifact key", maximum=512)
    if not re.fullmatch(r"[0-9A-Za-z._-]+", key):
        raise ValueError("Telegram artifact key contains unsafe characters")
    return os.path.join(PAYLOAD_DIR, f"{key}.payload"), os.path.join(PAYLOAD_DIR, f"{key}.artifact.json")


def materialize_artifact(label, payload, key):
    payload_path, manifest_path = artifact_paths(key)
    if payload is not None:
        atomic_bytes(payload_path, payload)
    atomic_json(manifest_path, {"label": label, "payload_path": payload_path})
    return manifest_path


def encode(value):
    media = value.get("media") if isinstance(value, dict) else None
    value = sanitize({key: item for key, item in value.items() if key != "media"} if media is not None else value)
    if media is not None:
        value["media"] = media
    return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":"), allow_nan=False).encode("utf-8")


def materialized_artifact_id(manifest_path, maximum):
    manifest = require_keys(json.loads(read_bounded_regular(manifest_path, 1048576, "materialized artifact manifest")), {"label", "payload_path"}, {"payload_content_hash"}, "Telegram materialized artifact manifest")
    payload = read_bounded_regular(manifest["payload_path"], maximum, "materialized artifact payload")
    pinned = {"label": manifest["label"], "payload_path": manifest["payload_path"], "payload_content_hash": octopus_content_hash(payload)}
    return f"{manifest['label']}:{octopus_content_hash(json.dumps(pinned, ensure_ascii=False, separators=(',', ':')).encode('utf-8'))}"


def find_admitted_arrival_seq(artifact_id, max_row_bytes):
    try:
        connection = sqlite3.connect(f"file:{urllib.parse.quote(os.path.abspath(LEDGER_INDEX_SQLITE))}?mode=ro", uri=True, timeout=0.25)
        try:
            if connection.execute("SELECT value FROM index_meta WHERE key = 'schema_version'").fetchone() != ("4",):
                raise RuntimeError("Telegram ledger index has unsupported schema")
            row = connection.execute("SELECT seq FROM movements WHERE kind = 'InterfaceArrival' AND from_kind = 'interface' AND from_id = ? AND artifact_id = ? ORDER BY seq ASC LIMIT 1", (INTERFACE_ID, artifact_id)).fetchone()
        finally:
            connection.close()
        if row is not None:
            return require_int(row[0], "Telegram admitted arrival seq", 0)
    except (OSError, RuntimeError, ValueError, sqlite3.DatabaseError):
        pass
    try:
        handle = open(MOVEMENTS_JSONL, "rb")
    except FileNotFoundError:
        return None
    with handle:
        while True:
            line = handle.readline(max_row_bytes + 2)
            if not line or not line.endswith(b"\n"):
                break
            if len(line) > max_row_bytes + 1:
                raise RuntimeError("Telegram movement lookup row exceeds configured ceiling")
            movement = require_object(json.loads(line), "Telegram movement lookup row")
            artifact = movement.get("artifact")
            if movement.get("kind") == "InterfaceArrival" and movement.get("from") == {"Interface": INTERFACE_ID} and isinstance(artifact, dict) and artifact.get("id") == artifact_id:
                return require_int(movement.get("seq"), "Telegram admitted arrival seq", 0)
    return None


def hand_idempotent(manifest_path, config):
    artifact_id = materialized_artifact_id(manifest_path, config["outbound"]["max_payload_bytes"])
    evidence = [{"interface_id": INTERFACE_ID, "artifact_id": artifact_id, "label": "telegram.arrival.idempotency", "bytes": list(hashlib.sha256(artifact_id.encode("utf-8")).digest())}]
    try:
        return hand_artifact(manifest_path, evidence)
    except ArtifactDeliveryRefused as error:
        refused = error.verdict.get("refused") if isinstance(error.verdict, dict) else None
        details = refused.get("detail") if isinstance(refused, dict) else None
        if isinstance(details, dict) and details.get("artifact_id") == artifact_id and details.get("message") == "arrival evidence already exists in the movement ledger":
            admitted = find_admitted_arrival_seq(artifact_id, config["ledger"]["max_row_bytes"])
            if admitted is not None:
                return admitted
        raise


def hand_json_idempotent(label, value, key, config):
    return hand_idempotent(materialize_artifact(label, encode(value), key), config)


def answered(key):
    return os.path.exists(artifact_paths(key)[0])


def hand_answer(label, value, key, config):
    return hand_idempotent(materialize_artifact(label, None if value is None else encode(value), key), config)


def request_path(key):
    artifact_paths(key)
    return os.path.join(REQUEST_DIR, f"{key}.json")


def load_request(key):
    value = read_json_file(request_path(key), 65536, f"request journal {key}")
    if value is not None and (not isinstance(value, dict) or value.get("schema_version") != 1):
        raise ValueError(f"Telegram request journal {key} is malformed")
    return value


def record_request(key, value):
    atomic_json(request_path(key), {"schema_version": 1, **sanitize(value)}, indent=2)


def forget_request(key):
    try:
        os.unlink(request_path(key))
    except FileNotFoundError:
        pass


def load_cursor():
    value = read_json_file(CURSOR_PATH, 65536, "ledger cursor")
    if value is None:
        return 0
    value = require_keys(value, {"schema_version", "next_seq"}, set(), "Telegram ledger cursor")
    if value["schema_version"] != 1:
        raise ValueError("Telegram ledger cursor schema_version must be 1")
    return require_int(value["next_seq"], "Telegram ledger cursor.next_seq", 0)


def commit_cursor(next_seq):
    atomic_json(CURSOR_PATH, {"schema_version": 1, "next_seq": next_seq})


def ledger_end_seq(max_row_bytes):
    try:
        descriptor = os.open(MOVEMENTS_JSONL, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0))
    except FileNotFoundError:
        return 0
    next_seq = 0
    with os.fdopen(descriptor, "rb") as handle:
        if not stat.S_ISREG(os.fstat(handle.fileno()).st_mode):
            raise RuntimeError("Telegram movement ledger is not a regular file")
        while True:
            row_offset = handle.tell()
            line = handle.readline(max_row_bytes + 1)
            if not line or not line.endswith(b"\n"):
                return next_seq
            if len(line) > max_row_bytes:
                raise RuntimeError(f"Telegram ledger row at byte {row_offset} exceeds {max_row_bytes} bytes")
            seq = require_int(require_object(json.loads(line), f"Telegram ledger movement").get("seq"), f"Telegram ledger movement.seq", 0)
            next_seq = max(next_seq, seq + 1)


def start_cursor(max_row_bytes):
    if not os.path.exists(CURSOR_PATH):
        commit_cursor(ledger_end_seq(max_row_bytes))


def content_hexdigest(content):
    if isinstance(content, list):
        if len(content) != 32 or any(isinstance(item, bool) or not isinstance(item, int) or item < 0 or item > 255 for item in content):
            raise ValueError("Telegram artifact content list is not a 32-byte hash")
        content = bytes(content)
    if not isinstance(content, bytes) or len(content) != 32:
        raise ValueError("Telegram artifact content must be a 32-byte hash")
    return content.hex()


def materialize_movement_payload(content, config):
    digest = content_hexdigest(content)
    manifest_bytes = read_bounded_regular(os.path.join(BLOBS_DIR, digest), config["outbound"]["max_manifest_bytes"], "cached artifact manifest")
    if octopus_content_hash(manifest_bytes) != digest:
        raise ValueError("Telegram cached artifact manifest failed content-address verification")
    manifest = require_object(json.loads(manifest_bytes), "Telegram cached artifact manifest")
    label = require_string(manifest.get("label"), "Telegram cached artifact label")
    payload_digest = manifest.get("payload_content_hash")
    if not isinstance(payload_digest, str) or not re.fullmatch(r"[0-9a-f]{64}", payload_digest):
        raise ValueError("Telegram cached artifact manifest has no payload hash")
    payload = read_bounded_regular(os.path.join(BLOBS_DIR, payload_digest), config["outbound"]["max_payload_bytes"], "cached artifact payload")
    if octopus_content_hash(payload) != payload_digest:
        raise ValueError("Telegram cached artifact payload failed content-address verification")
    return label, payload, digest


def artifact_blob(blob_id, maximum):
    if not isinstance(blob_id, str) or not re.fullmatch(r"[0-9a-f]{64}", blob_id):
        raise ValueError("Telegram file blob_id must be a content hash")
    content = read_bounded_regular(os.path.join(BLOBS_DIR, blob_id), maximum, "file upload blob")
    if octopus_content_hash(content) != blob_id:
        raise ValueError("Telegram file upload blob failed content-address verification")
    return content


def movement_interface_endpoint(value):
    if not isinstance(value, dict) or set(value) != {"Interface"}:
        return None
    identifier = value.get("Interface")
    if not isinstance(identifier, str) or not identifier or identifier.strip() != identifier:
        return None
    return identifier


def require_outbound_runtime(movement):
    runtime = require_object(require_object(movement.get("invocation_metadata"), "Telegram outbound movement.invocation_metadata").get("runtime"), "Telegram outbound movement runtime metadata")
    for field in ("run", "ee", "worker", "checkpoint", "interface"):
        require_string(runtime.get(field), f"Telegram outbound movement runtime.{field}")
    require_int(runtime.get("epoch"), "Telegram outbound movement runtime.epoch", 0)
    if runtime["interface"] != INTERFACE_ID:
        raise ValueError("Telegram outbound runtime interface does not match its exact target")
    return runtime


def read_index(next_seq, limit):
    connection = sqlite3.connect(f"file:{urllib.parse.quote(os.path.abspath(LEDGER_INDEX_SQLITE))}?mode=ro", uri=True, timeout=0.25)
    try:
        if connection.execute("SELECT value FROM index_meta WHERE key = 'schema_version'").fetchone() != ("4",):
            raise RuntimeError("Telegram ledger index schema is unsupported")
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
        seq = require_int(seq, "Telegram ledger index seq", 0)
        require_string(source_interface, f"Telegram ledger index movement {seq}.from_id")
        if content is not None and (not isinstance(content, str) or not re.fullmatch(r"[0-9a-f]{64}", content)):
            raise RuntimeError(f"Telegram ledger index movement {seq} has malformed artifact hash")
        require_string(run, f"Telegram ledger index movement {seq}.run")
        require_string(checkpoint, f"Telegram ledger index movement {seq}.checkpoint")
        result.append((seq, bytes.fromhex(content) if content else None, run))
    return result


def read_ledger(next_seq, limit, max_row_bytes):
    max_row_bytes = require_int(max_row_bytes, "Telegram ledger.max_row_bytes", 256, 16777216)
    with _LEDGER_LOCK:
        try:
            descriptor = os.open(MOVEMENTS_JSONL, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0))
        except FileNotFoundError:
            return []
        result = []
        with os.fdopen(descriptor, "rb") as handle:
            status = os.fstat(handle.fileno())
            if not stat.S_ISREG(status.st_mode):
                raise RuntimeError("Telegram movement ledger is not a regular file")
            identity = (status.st_dev, status.st_ino)
            if identity != _LEDGER_FALLBACK["identity"] or status.st_size < _LEDGER_FALLBACK["size"] or _LEDGER_FALLBACK["offset"] > status.st_size:
                _LEDGER_FALLBACK["offset"] = 0
            handle.seek(_LEDGER_FALLBACK["offset"])
            safe_offset = _LEDGER_FALLBACK["offset"]
            pending = False
            while True:
                row_offset = handle.tell()
                line = handle.readline(max_row_bytes + 1)
                if not line:
                    if not pending:
                        safe_offset = handle.tell()
                    break
                if len(line) > max_row_bytes:
                    raise RuntimeError(f"Telegram ledger row at byte {row_offset} exceeds {max_row_bytes} bytes")
                if not line.endswith(b"\n"):
                    raise RuntimeError(f"Telegram ledger row at byte {row_offset} is partial")
                movement = require_object(json.loads(line), "Telegram ledger movement")
                seq = require_int(movement.get("seq"), "Telegram ledger movement.seq", 0)
                relevant = seq >= next_seq and movement.get("kind") == "InterfaceOutbound" and movement_interface_endpoint(movement.get("to")) == INTERFACE_ID and movement_interface_endpoint(movement.get("from")) is not None
                if not relevant:
                    if not pending:
                        safe_offset = handle.tell()
                    continue
                runtime = require_outbound_runtime(movement)
                artifact = movement.get("artifact") if isinstance(movement.get("artifact"), dict) else None
                if not pending:
                    safe_offset = row_offset
                    pending = True
                result.append((seq, artifact.get("content") if artifact else None, runtime.get("run")))
                if len(result) >= limit:
                    break
            _LEDGER_FALLBACK["offset"] = safe_offset
            _LEDGER_FALLBACK["size"] = os.fstat(handle.fileno()).st_size
            _LEDGER_FALLBACK["identity"] = identity
        return result


def read_outbound(next_seq, config, limit=128):
    try:
        return read_index(next_seq, limit)
    except (OSError, RuntimeError, ValueError, sqlite3.DatabaseError) as error:
        stderr(f"Telegram ledger index unavailable; reading ledger truth: {error}", key=f"ledger-index:{type(error).__name__}")
        return read_ledger(next_seq, limit, config["ledger"]["max_row_bytes"])


for _directory in (VAR_DIR, PAYLOAD_DIR, REQUEST_DIR):
    ensure_private_directory(_directory)
