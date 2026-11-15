#!/usr/bin/env python3
import datetime
import json
import os
import re
import stat
import sys
import time

from octopus_content_hash import octopus_content_hash


INTERFACE_ID = os.environ["OCTOPUS_INTERFACE_ID"]
INTERFACE_DIR = os.path.abspath(os.environ["OCTOPUS_INTERFACE_DIR"])
VAR_DIR = os.path.abspath(os.environ["OCTOPUS_VAR_DIR"])
MOVEMENTS_JSONL = os.path.abspath(os.environ["OCTOPUS_MOVEMENTS_JSONL"])
BLOBS_DIR = os.path.abspath(os.environ["OCTOPUS_BLOBS_DIR"])
WATCH_PATH = os.path.join(INTERFACE_DIR, "watch.json")
TRIGGER_EVENT = "A new letter arrived in the owner's mailbox, and it contains words this trigger watches for. The trigger fires once."
ARMED_PATH = os.path.join(INTERFACE_DIR, "armed.json")
CURSOR_PATH = os.path.join(VAR_DIR, "ledger-cursor.json")
STATE_PATH = os.path.join(VAR_DIR, "fired.json")
PAYLOADS_DIR = os.path.join(VAR_DIR, "payloads")
EVENT_LABEL = "email.event"
EVIDENCE_LABEL = "legacy_watch.arrival.idempotency"
DUPLICATE_MESSAGE = "arrival evidence already exists in the movement ledger"
MATCH_FIELDS = {"subject", "body"}
HEX_HASH = re.compile(r"[0-9a-f]{64}")
POLL_SECONDS = 0.25
RETRY_FIRST_SECONDS = 1
RETRY_MAX_SECONDS = 30
MAX_WATCH_BYTES = 65536
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


def log(message):
    sys.stderr.write(f"legacy_watch {INTERFACE_ID}: {message}\n")
    sys.stderr.flush()


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


def optional_text(value, name):
    if value is not None and not isinstance(value, str):
        raise ValueError(f"{name} must be a string or null")
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


def read_private_json(path, name):
    try:
        body = stable_read(path, IO_LIMITS["max_control_frame_bytes"], name, private=True)
    except FileNotFoundError:
        return None
    return json.loads(body)


def load_watch():
    watch = require_keys(
        json.loads(stable_read(WATCH_PATH, MAX_WATCH_BYTES, "watch.json")),
        {"terms", "match_fields", "instructions", "notice_label"},
        {"event"},
        "watch.json",
    )
    require_string(watch.setdefault("event", TRIGGER_EVENT), "watch.json event")
    terms = watch["terms"]
    if not isinstance(terms, list) or not terms:
        raise ValueError("watch.json terms must be a non-empty list")
    normalized = []
    for index, term in enumerate(terms):
        require_string(term, f"watch.json terms[{index}]", 1024)
        normalized.append(normalized_text(term))
    fields = watch["match_fields"]
    if (
        not isinstance(fields, list)
        or not fields
        or len(set(fields)) != len(fields)
        or any(field not in MATCH_FIELDS for field in fields)
    ):
        raise ValueError("watch.json match_fields must be a non-empty unique list of subject and/or body")
    if not isinstance(watch["instructions"], str):
        raise ValueError("watch.json instructions must be a string")
    require_string(watch["notice_label"], "watch.json notice_label", 256)
    return {**watch, "normalized_terms": normalized}


def normalized_text(value):
    return " ".join(value.casefold().split())


def aware_time(value, name):
    if not isinstance(value, str):
        raise ValueError(f"{name} must be an ISO 8601 time string")
    try:
        parsed = datetime.datetime.fromisoformat(value)
    except ValueError:
        raise ValueError(f"{name} is not an ISO 8601 time: {value!r}") from None
    if parsed.tzinfo is None or parsed.utcoffset() is None:
        raise ValueError(f"{name} carries no UTC offset: {value!r}")
    return parsed


def load_armed():
    try:
        body = stable_read(ARMED_PATH, MAX_WATCH_BYTES, "armed.json")
    except FileNotFoundError:
        raise RuntimeError(f"this trigger is not armed: {ARMED_PATH} is missing; arming writes it with armed_at") from None
    armed = require_keys(json.loads(body), {"armed_at"}, set(), "armed.json")
    return aware_time(armed["armed_at"], "armed.json armed_at")


def empty_cursor():
    return {"schema_version": 1, "ledger_device": None, "ledger_inode": None, "byte_offset": 0, "next_seq": 0}


def validate_cursor(value):
    value = require_keys(
        value,
        {"schema_version", "ledger_device", "ledger_inode", "byte_offset", "next_seq"},
        set(),
        "ledger cursor",
    )
    if value["schema_version"] != 1:
        raise ValueError("ledger cursor schema_version is unsupported")
    for key in ("ledger_device", "ledger_inode"):
        if value[key] is not None:
            require_int(value[key], f"ledger cursor.{key}")
    if (value["ledger_device"] is None) != (value["ledger_inode"] is None):
        raise ValueError("ledger cursor identity is incomplete")
    require_int(value["byte_offset"], "ledger cursor.byte_offset")
    require_int(value["next_seq"], "ledger cursor.next_seq")
    return value


def commit_cursor(cursor):
    atomic_json(CURSOR_PATH, validate_cursor(cursor))


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


def load_cursor():
    value = read_private_json(CURSOR_PATH, "ledger cursor")
    return empty_cursor() if value is None else validate_cursor(value)


def interface_endpoint(value):
    if not isinstance(value, dict) or set(value) != {"Interface"}:
        return None
    identifier = value.get("Interface")
    if not isinstance(identifier, str) or not identifier or identifier.strip() != identifier:
        return None
    return identifier


def is_own_outbound(movement):
    if movement.get("kind") != "InterfaceOutbound" or interface_endpoint(movement.get("to")) != INTERFACE_ID:
        return False
    sequence = movement["seq"]
    if interface_endpoint(movement.get("from")) is None:
        raise ValueError(f"outbound movement {sequence} has no interface source")
    metadata = require_object(movement.get("invocation_metadata"), f"outbound movement {sequence}.invocation_metadata")
    runtime = require_object(metadata.get("runtime"), f"outbound movement {sequence}.invocation_metadata.runtime")
    for field in ("run", "ee", "worker", "checkpoint", "interface"):
        require_string(runtime.get(field), f"outbound movement {sequence} runtime.{field}", 256)
    require_int(runtime.get("epoch"), f"outbound movement {sequence} runtime.epoch")
    if runtime["interface"] != INTERFACE_ID:
        raise ValueError(f"outbound movement {sequence} runtime interface does not match its exact target")
    return True


def content_hexdigest(content, name):
    if isinstance(content, list):
        if len(content) != 32 or any(isinstance(item, bool) or not isinstance(item, int) or not 0 <= item <= 255 for item in content):
            raise ValueError(f"{name} is not a 32-byte hash")
        return bytes(content).hex()
    if isinstance(content, str) and HEX_HASH.fullmatch(content):
        return content
    raise ValueError(f"{name} is not a 32-byte hash")


def read_blob(digest, maximum, name):
    content = stable_read(os.path.join(BLOBS_DIR, digest), maximum, name)
    if octopus_content_hash(content) != digest:
        raise ValueError(f"{name} {digest} failed its content hash check")
    return content


def read_artifact(movement):
    sequence = movement["seq"]
    artifact = require_object(movement.get("artifact"), f"outbound movement {sequence}.artifact")
    manifest_hash = content_hexdigest(artifact.get("content"), f"outbound movement {sequence} artifact content")
    try:
        manifest = require_keys(
            json.loads(read_blob(manifest_hash, IO_LIMITS["max_manifest_bytes"], "artifact manifest")),
            {"label", "payload_path", "payload_content_hash"},
            set(),
            f"outbound movement {sequence} artifact manifest",
        )
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise ValueError(f"outbound movement {sequence} artifact manifest is not JSON: {error}") from None
    label = require_string(manifest["label"], f"outbound movement {sequence} artifact label", 256)
    payload_hash = require_hash(manifest["payload_content_hash"], f"outbound movement {sequence} payload_content_hash")
    return label, payload_hash


def read_letter(sequence, payload_hash):
    payload = read_blob(payload_hash, IO_LIMITS["max_payload_bytes"], "artifact payload")
    try:
        event = require_object(json.loads(payload), f"email event {sequence}")
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise ValueError(f"email event {sequence} is not JSON: {error}") from None
    message = require_object(event.get("message"), f"email event {sequence}.message")
    body = require_object(message.get("body"), f"email event {sequence}.message.body")
    text = body.get("text")
    if not isinstance(text, str):
        raise ValueError(f"email event {sequence}.message.body.text must be a string")
    if "received_at" not in message:
        raise ValueError(f"email event {sequence}.message has no received_at")
    received = message["received_at"]
    received = None if received is None else aware_time(received, f"email event {sequence}.message.received_at")
    letter = {
        "subject": optional_text(message.get("subject"), f"email event {sequence}.message.subject"),
        "date": optional_text(message.get("date"), f"email event {sequence}.message.date"),
        "body": text,
    }
    return letter, received


def matched_terms(watch, letter):
    haystacks = [normalized_text(letter[field] or "") for field in watch["match_fields"]]
    return [
        term
        for term, normalized in zip(watch["terms"], watch["normalized_terms"])
        if any(normalized in haystack for haystack in haystacks)
    ]


def private_file(path, content, maximum, name):
    try:
        existing = stable_read(path, maximum, name, private=True)
    except FileNotFoundError:
        atomic_bytes(path, content)
        return
    if existing != content:
        raise RuntimeError(f"{name} conflicts with its content identity: {path}")


def prepare_notice(watch, letter, matched):
    payload = canonical_json({
        "trigger": {
            "event": watch["event"],
            "watched_fields": watch["match_fields"],
            "watched_words": watch["terms"],
            "matched_words": matched,
        },
        "from_owner": watch["instructions"],
        "message": letter,
    })
    if len(payload) > IO_LIMITS["max_payload_bytes"]:
        raise ValueError("the notice payload exceeds max_payload_bytes")
    payload_hash = octopus_content_hash(payload)
    payload_path = os.path.join(PAYLOADS_DIR, f"notice-{payload_hash}.payload")
    private_file(payload_path, payload, IO_LIMITS["max_payload_bytes"], "notice payload")
    manifest = canonical_json(
        {"label": watch["notice_label"], "payload_path": payload_path, "payload_content_hash": payload_hash}
    )
    if len(manifest) > IO_LIMITS["max_manifest_bytes"]:
        raise ValueError("the notice manifest exceeds max_manifest_bytes")
    manifest_hash = octopus_content_hash(manifest)
    manifest_path = os.path.join(PAYLOADS_DIR, f"notice-{manifest_hash}.artifact.json")
    private_file(manifest_path, manifest, IO_LIMITS["max_manifest_bytes"], "notice manifest")
    return manifest_path, manifest_hash


def load_state():
    value = read_private_json(STATE_PATH, "fired state")
    if value is None:
        return None
    fields = {"schema_version", "status", "label", "manifest_path", "manifest_hash", "source_movement_seq", "admitted_seq", "refusal"}
    state = require_keys(value, fields, set(), "fired state")
    if state["schema_version"] != 1 or state["status"] not in {"prepared", "admitted", "refused"}:
        raise ValueError("fired state has an unsupported schema or status")
    require_string(state["label"], "fired state.label", 256)
    require_hash(state["manifest_hash"], "fired state.manifest_hash")
    require_int(state["source_movement_seq"], "fired state.source_movement_seq")
    manifest_path = require_string(state["manifest_path"], "fired state.manifest_path")
    if os.path.dirname(manifest_path) != PAYLOADS_DIR:
        raise ValueError("fired state manifest_path is outside this sensor's payload directory")
    if state["status"] != "admitted" and state["admitted_seq"] is not None:
        raise ValueError(f"{state['status']} fired state carries admitted_seq")
    if state["status"] == "admitted":
        require_int(state["admitted_seq"], "fired state.admitted_seq")
    if state["status"] == "refused":
        require_keys(state["refusal"], {"reason", "detail"}, set(), "fired state.refusal")
    elif state["refusal"] is not None:
        raise ValueError(f"{state['status']} fired state carries a refusal")
    return state


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


def artifact_id_of(state):
    return f"{state['label']}:{state['manifest_hash']}"


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


def deliver(state):
    artifact_id = artifact_id_of(state)
    evidence = [{
        "interface_id": INTERFACE_ID,
        "artifact_id": artifact_id,
        "label": EVIDENCE_LABEL,
        "bytes": list(bytes.fromhex(octopus_content_hash(artifact_id.encode("utf-8")))),
    }]
    admitted = find_admitted_arrival_seq(artifact_id)
    delay = RETRY_FIRST_SECONDS
    while admitted is None:
        try:
            admitted = hand(state["manifest_path"], evidence)
        except Refused as refusal:
            if refusal.duplicate_of(artifact_id):
                admitted = find_admitted_arrival_seq(artifact_id)
                if admitted is None:
                    raise RuntimeError("the runtime reports the notice as already handed, but the ledger has no such arrival") from None
            elif refusal.transient():
                log(f"{refusal}; the same notice is handed again in {delay}s")
                time.sleep(delay)
                delay = min(delay * 2, RETRY_MAX_SECONDS)
            else:
                state = {**state, "status": "refused", "refusal": refusal.record()}
                atomic_json(STATE_PATH, state)
                log(f"{refusal}; this trigger is spent without a delivery and stays quiet until it is armed again (a new armed.json and no fired.json)")
                return state
    state = {**state, "status": "admitted", "admitted_seq": admitted}
    atomic_json(STATE_PATH, state)
    log(f"the notice {artifact_id} was admitted at ledger seq {admitted}; this trigger has fired and stays quiet")
    return state


def process_outbound(movement, watch, armed_at):
    sequence = movement["seq"]
    label, payload_hash = read_artifact(movement)
    if label != EVENT_LABEL:
        log(f"movement {sequence} carries {label}, not {EVENT_LABEL}; it is not a letter and is passed over")
        return None
    letter, received = read_letter(sequence, payload_hash)
    if received is None:
        return "undated"
    if received < armed_at:
        return "early"
    matched = matched_terms(watch, letter)
    if not matched:
        return None
    manifest_path, manifest_hash = prepare_notice(watch, letter, matched)
    state = {
        "schema_version": 1,
        "status": "prepared",
        "label": watch["notice_label"],
        "manifest_path": manifest_path,
        "manifest_hash": manifest_hash,
        "source_movement_seq": sequence,
        "admitted_seq": None,
        "refusal": None,
    }
    atomic_json(STATE_PATH, state)
    log(f"letter at ledger seq {sequence} matched a watched term; handing {watch['notice_label']}")
    return deliver(state)


def follow(cursor, watch, armed_at, state):
    passed = {"early": 0, "undated": 0}
    for movement, candidate in movements_after(cursor):
        if movement is not None and state is None and is_own_outbound(movement):
            outcome = process_outbound(movement, watch, armed_at)
            if isinstance(outcome, dict):
                state = outcome
            elif outcome in passed:
                passed[outcome] += 1
        if candidate != cursor:
            commit_cursor(candidate)
            cursor = candidate
    if passed["early"] or passed["undated"]:
        log(
            f"passed over {passed['early']} letters received before {armed_at.isoformat()} "
            f"and {passed['undated']} letters without a received time; cursor at seq {cursor['next_seq']}"
        )
    return cursor, state


def main():
    os.umask(0o077)
    for path in (VAR_DIR, PAYLOADS_DIR):
        private_directory(path)
    watch = load_watch()
    armed_at = load_armed()
    state = load_state()
    cursor = load_cursor()
    if state is not None and state["status"] == "admitted":
        if find_admitted_arrival_seq(artifact_id_of(state), state["admitted_seq"]) is None:
            raise RuntimeError(
                f"fired.json records an admission at ledger seq {state['admitted_seq']}, "
                "but the movement ledger holds no such arrival from this sensor"
            )
    status = state["status"] if state else "armed"
    log(
        f"up: {status}, letters received from {armed_at.isoformat()} on, cursor at seq {cursor['next_seq']}, "
        f"{len(watch['terms'])} terms in {', '.join(watch['match_fields'])}"
    )
    signal_ready()
    if state is not None and state["status"] == "prepared":
        verify_prepared(state)
        log(f"handing the prepared notice from ledger seq {state['source_movement_seq']} again")
        state = deliver(state)
    while True:
        cursor, state = follow(cursor, watch, armed_at, state)
        time.sleep(POLL_SECONDS)


if __name__ == "__main__":
    try:
        main()
    except Exception as error:
        log(f"stopped: {error}")
        raise SystemExit(1)
