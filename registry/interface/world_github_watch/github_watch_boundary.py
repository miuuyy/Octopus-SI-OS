#!/usr/bin/env python3
"""Boundary helpers for the GitHub watch life: private files, the declared
stdin/stdout arrival protocol and read-only ledger lookup of its own arrivals."""
import json
import os
import re
import stat
import sys
import time

from octopus_content_hash import octopus_content_hash


INTERFACE_ID = os.environ["OCTOPUS_INTERFACE_ID"]
VAR_DIR = os.path.abspath(os.environ["OCTOPUS_VAR_DIR"])
MOVEMENTS_JSONL = os.path.abspath(os.environ["OCTOPUS_MOVEMENTS_JSONL"])
PAYLOADS_DIR = os.path.join(VAR_DIR, "payloads")
DUPLICATE_MESSAGE = "arrival evidence already exists in the movement ledger"
HEX_HASH = re.compile(r"[0-9a-f]{64}")
MAX_REFUSAL_DETAIL_BYTES = 16384
_NEXT_REQUEST_ID = 1

class Refused(RuntimeError):
    def __init__(self, verdict):
        refused = verdict.get("refused") if isinstance(verdict.get("refused"), dict) else {}
        self.reason = refused.get("reason")
        self.detail = refused.get("detail") if isinstance(refused.get("detail"), dict) else {}
        super().__init__(
            f"the runtime refused the notice ({self.reason}): "
            f"{json.dumps(self.detail, ensure_ascii=False, sort_keys=True)[:2048]}"
        )

    def transient(self):
        return self.reason == "registry_unstable" or (
            self.reason == "stale_epoch" and self.detail.get("reason_detail") == "arrival_target_changed"
        )

    def duplicate_of(self, artifact_id):
        return self.detail.get("artifact_id") == artifact_id and self.detail.get("message") == DUPLICATE_MESSAGE

    def record(self):
        detail = self.detail
        encoded = canonical_json(detail)
        if len(encoded) > MAX_REFUSAL_DETAIL_BYTES:
            detail = {"truncated": encoded[:MAX_REFUSAL_DETAIL_BYTES].decode("utf-8", "ignore")}
        return {"reason": self.reason if isinstance(self.reason, str) else None, "detail": detail}


def require_object(value, name):
    if not isinstance(value, dict):
        raise ValueError(f"{name} must be a JSON object")
    return value


def require_keys(value, required, optional, name):
    value = require_object(value, name)
    missing = sorted(set(required) - set(value))
    unknown = sorted(set(value) - set(required) - set(optional))
    if missing:
        raise ValueError(f"{name} is missing keys: {', '.join(missing)}")
    if unknown:
        raise ValueError(f"{name} has unknown keys: {', '.join(unknown)}")
    return value


def require_int(value, name, minimum=0):
    if isinstance(value, bool) or not isinstance(value, int) or value < minimum:
        raise ValueError(f"{name} must be an integer >= {minimum}")
    return value


def require_string(value, name, maximum=4096):
    if not isinstance(value, str) or not value or value.strip() != value:
        raise ValueError(f"{name} must be a non-empty trimmed string")
    if len(value.encode("utf-8")) > maximum:
        raise ValueError(f"{name} exceeds {maximum} bytes")
    return value


def require_hash(value, name):
    if not isinstance(value, str) or HEX_HASH.fullmatch(value) is None:
        raise ValueError(f"{name} must be a 64-character lowercase hex content hash")
    return value


def load_io_limits():
    value = require_keys(
        json.loads(os.environ["OCTOPUS_INTERFACE_HOST_IO_LIMITS"]),
        {"max_control_frame_bytes", "max_manifest_bytes", "max_payload_bytes", "max_stderr_bytes"},
        set(),
        "host IO limits",
    )
    return {key: require_int(value[key], f"host IO limits.{key}", 1) for key in value}


IO_LIMITS = load_io_limits()


def private_directory(path):
    os.makedirs(path, mode=0o700, exist_ok=True)
    state = os.lstat(path)
    if stat.S_ISLNK(state.st_mode) or not stat.S_ISDIR(state.st_mode):
        raise PermissionError(f"private path is not a directory: {path}")
    if hasattr(os, "geteuid") and state.st_uid != os.geteuid():
        raise PermissionError(f"private path has the wrong owner: {path}")
    if stat.S_IMODE(state.st_mode) != 0o700:
        os.chmod(path, 0o700)
    return path


def stable_read(path, maximum, name, private=False):
    before = os.lstat(path)
    if stat.S_ISLNK(before.st_mode) or not stat.S_ISREG(before.st_mode):
        raise PermissionError(f"{name} is not a regular file: {path}")
    if private:
        if stat.S_IMODE(before.st_mode) != 0o600:
            raise PermissionError(f"{name} is not private: {path}")
        if hasattr(os, "geteuid") and before.st_uid != os.geteuid():
            raise PermissionError(f"{name} has the wrong owner: {path}")
    if before.st_size > maximum:
        raise ValueError(f"{name} exceeds {maximum} bytes: {path}")
    descriptor = os.open(path, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0))
    try:
        opened = os.fstat(descriptor)
        if (before.st_dev, before.st_ino) != (opened.st_dev, opened.st_ino) or not stat.S_ISREG(opened.st_mode):
            raise PermissionError(f"{name} changed while opening: {path}")
        with os.fdopen(descriptor, "rb") as handle:
            descriptor = -1
            value = handle.read(maximum + 1)
        if len(value) > maximum:
            raise ValueError(f"{name} grew beyond {maximum} bytes: {path}")
        return value
    finally:
        if descriptor >= 0:
            os.close(descriptor)


def sync_directory(path):
    descriptor = os.open(path, os.O_RDONLY)
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def atomic_bytes(path, value):
    private_directory(os.path.dirname(path))
    temporary = f"{path}.{os.getpid()}.{time.time_ns()}.tmp"
    descriptor = os.open(
        temporary,
        os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_NOFOLLOW", 0),
        0o600,
    )
    try:
        with os.fdopen(descriptor, "wb") as handle:
            descriptor = -1
            handle.write(value)
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(temporary, path)
        os.chmod(path, 0o600)
        sync_directory(os.path.dirname(path))
    finally:
        if descriptor >= 0:
            os.close(descriptor)
        try:
            os.unlink(temporary)
        except FileNotFoundError:
            pass


def canonical_json(value):
    return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode("utf-8")


def atomic_json(path, value):
    body = canonical_json(value) + b"\n"
    if len(body) > IO_LIMITS["max_control_frame_bytes"]:
        raise ValueError(f"private JSON exceeds max_control_frame_bytes: {path}")
    atomic_bytes(path, body)


def empty_cursor():
    return {"schema_version": 1, "ledger_device": None, "ledger_inode": None, "byte_offset": 0, "next_seq": 0}


def open_ledger():
    before = os.lstat(MOVEMENTS_JSONL)
    if stat.S_ISLNK(before.st_mode) or not stat.S_ISREG(before.st_mode):
        raise PermissionError("the movement ledger is not a regular file")
    descriptor = os.open(MOVEMENTS_JSONL, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0))
    opened = os.fstat(descriptor)
    if not stat.S_ISREG(opened.st_mode) or (before.st_dev, before.st_ino) != (opened.st_dev, opened.st_ino):
        os.close(descriptor)
        raise PermissionError("the movement ledger changed while opening")
    return os.fdopen(descriptor, "rb"), opened


def movements_after(cursor):
    try:
        handle, state = open_ledger()
    except FileNotFoundError:
        yield None, dict(cursor)
        return
    with handle:
        identity = (state.st_dev, state.st_ino)
        reset = (cursor["ledger_device"], cursor["ledger_inode"]) != identity or state.st_size < cursor["byte_offset"]
        candidate = dict(cursor)
        candidate["ledger_device"], candidate["ledger_inode"] = identity
        candidate["byte_offset"] = 0 if reset else cursor["byte_offset"]
        handle.seek(candidate["byte_offset"])
        previous = None
        while True:
            start = handle.tell()
            line = handle.readline(IO_LIMITS["max_control_frame_bytes"] + 1)
            if len(line) > IO_LIMITS["max_control_frame_bytes"]:
                raise RuntimeError(f"ledger row at byte {start} exceeds max_control_frame_bytes")
            if not line or not line.endswith(b"\n"):
                yield None, dict(candidate)
                return
            try:
                movement = require_object(json.loads(line), f"ledger row at byte {start}")
            except (UnicodeDecodeError, json.JSONDecodeError) as error:
                raise ValueError(f"ledger row at byte {start} is not JSON: {error}") from None
            sequence = require_int(movement.get("seq"), f"ledger row at byte {start}.seq")
            if previous is not None and sequence != previous + 1:
                raise RuntimeError(f"ledger sequence discontinuity after {previous}: {sequence}")
            previous = sequence
            if sequence < cursor["next_seq"]:
                if not reset:
                    raise RuntimeError("the cursor points behind an already committed movement")
                candidate["byte_offset"] = handle.tell()
                continue
            if sequence != candidate["next_seq"]:
                raise RuntimeError(f"ledger sequence discontinuity: expected {candidate['next_seq']}, got {sequence}")
            candidate["byte_offset"] = handle.tell()
            candidate["next_seq"] = sequence + 1
            yield movement, dict(candidate)


def private_file(path, content, maximum, name):
    try:
        existing = stable_read(path, maximum, name, private=True)
    except FileNotFoundError:
        atomic_bytes(path, content)
        return
    if existing != content:
        raise RuntimeError(f"{name} conflicts with its content identity: {path}")


def verify_prepared(state):
    manifest = stable_read(state["manifest_path"], IO_LIMITS["max_manifest_bytes"], "prepared notice manifest", private=True)
    if octopus_content_hash(manifest) != state["manifest_hash"]:
        raise ValueError("the prepared notice manifest no longer matches its recorded hash")
    descriptor = require_keys(json.loads(manifest), {"label", "payload_path", "payload_content_hash"}, set(), "prepared notice manifest")
    if descriptor["label"] != state["label"]:
        raise ValueError("the prepared notice manifest label differs from its recorded label")
    payload = stable_read(descriptor["payload_path"], IO_LIMITS["max_payload_bytes"], "prepared notice payload", private=True)
    if octopus_content_hash(payload) != descriptor["payload_content_hash"]:
        raise ValueError("the prepared notice payload no longer matches its recorded hash")


def signal_ready():
    write_frame({"kind": "ready"})


def write_frame(value):
    encoded = canonical_json(value) + b"\n"
    if len(encoded) > IO_LIMITS["max_control_frame_bytes"]:
        raise RuntimeError("control frame exceeds max_control_frame_bytes")
    sys.stdout.buffer.write(encoded)
    sys.stdout.buffer.flush()


def hand(manifest_path, evidence):
    global _NEXT_REQUEST_ID
    request_id = _NEXT_REQUEST_ID
    _NEXT_REQUEST_ID += 1
    write_frame({"kind": "arrival", "request_id": request_id, "artifact_manifest_path": manifest_path, "evidence": evidence})
    line = sys.stdin.buffer.readline(IO_LIMITS["max_control_frame_bytes"] + 1)
    if not line:
        raise RuntimeError("runtime verdict input closed before the notice was admitted")
    if len(line) > IO_LIMITS["max_control_frame_bytes"] or not line.endswith(b"\n"):
        raise RuntimeError("runtime verdict is oversized or partial")
    response = require_keys(json.loads(line), {"kind", "request_id", "verdict"}, set(), "runtime verdict")
    if response["kind"] != "delivery_verdict" or response["request_id"] != request_id:
        raise RuntimeError("runtime verdict does not match the pending notice")
    verdict = require_object(response["verdict"], "runtime delivery verdict")
    if set(verdict) == {"refused"}:
        raise Refused(verdict)
    if set(verdict) != {"admitted"}:
        raise RuntimeError("runtime delivery verdict has an invalid shape")
    admitted = require_keys(verdict["admitted"], {"seq"}, set(), "admitted verdict")
    return require_int(admitted["seq"], "admitted verdict.seq")


def find_admitted_arrival_seq(artifact_id, expected_seq=None):
    for movement, _ in movements_after(empty_cursor()):
        if movement is None:
            return None
        if expected_seq is not None and movement["seq"] > expected_seq:
            return None
        if movement.get("kind") != "InterfaceArrival" or movement.get("from") != {"Interface": INTERFACE_ID}:
            continue
        artifact = movement.get("artifact")
        if not isinstance(artifact, dict) or artifact.get("id") != artifact_id:
            continue
        if expected_seq is None or movement["seq"] == expected_seq:
            return movement["seq"]
    return None
