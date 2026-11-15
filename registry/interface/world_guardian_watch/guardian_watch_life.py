#!/usr/bin/env python3
# Quiet-period guardian sensor.
#
# Folds the movement ledger and remembers the last artifact that the watched
# run's provider interface emitted for that run (its "own action": a tool call,
# a message, a result). Arrivals to the run never count as life. When the run
# has emitted no own action for longer than quiet_seconds, one alert artifact
# is handed to the runtime; the same silence is reported again only after a
# growing cooldown. Everything it knows is in guardian_config.json (its private
# var directory first, its folder otherwise) and guardian-state.json (its
# private var directory).
import datetime
import importlib.util
import json
import os
import stat
import sys
import time


INTERFACE_ID = os.environ["OCTOPUS_INTERFACE_ID"]
INTERFACE_DIR = os.path.abspath(os.environ["OCTOPUS_INTERFACE_DIR"])
VAR_DIR = os.path.abspath(os.environ["OCTOPUS_VAR_DIR"])
MOVEMENTS_JSONL = os.path.abspath(os.environ["OCTOPUS_MOVEMENTS_JSONL"])
HASH_MODULE_PATH = os.path.join(os.path.dirname(INTERFACE_DIR), "world_cron", "octopus_content_hash.py")
CONFIG_NAME = "guardian_config.json"
REGISTRY_CONFIG_PATH = os.path.join(INTERFACE_DIR, CONFIG_NAME)
PRIVATE_CONFIG_PATH = os.path.join(VAR_DIR, CONFIG_NAME)
# The values the packaged guardian_config.json ships with. They name no run.
TEMPLATE_VALUES = {"watched_run": "RUN_ID", "provider_interface": "PROVIDER_INTERFACE_ID"}
STATE_PATH = os.path.join(VAR_DIR, "guardian-state.json")
PAYLOADS_DIR = os.path.join(VAR_DIR, "payloads")
EVIDENCE_LABEL = "guardian_watch.arrival.idempotency"
DUPLICATE_MESSAGE = "arrival evidence already exists in the movement ledger"
MAX_CONFIG_BYTES = 262144
MAX_RUN_RECORD_BYTES = 67108864
MAX_LOG_LINES = 300
MAX_LOG_LINE_CHARS = 600
RECENT_LIMIT = 12
TRANSIENT_ATTEMPTS = 8
_NEXT_REQUEST_ID = 1
_LOG_LINES = 0


def load_content_hash():
    spec = importlib.util.spec_from_file_location("octopus_guardian_content_hash", HASH_MODULE_PATH)
    if spec is None or spec.loader is None:
        raise RuntimeError("the content-hash dependency cannot be loaded: " + HASH_MODULE_PATH)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module.octopus_content_hash


octopus_content_hash = load_content_hash()


def load_io_limits():
    value = json.loads(os.environ["OCTOPUS_INTERFACE_HOST_IO_LIMITS"])
    if not isinstance(value, dict):
        raise ValueError("host IO limits must be a JSON object")
    limits = {}
    for key in ("max_control_frame_bytes", "max_manifest_bytes", "max_payload_bytes", "max_stderr_bytes"):
        number = value.get(key)
        if isinstance(number, bool) or not isinstance(number, int) or number < 1:
            raise ValueError("host IO limits." + key + " must be a positive integer")
        limits[key] = number
    return limits


IO_LIMITS = load_io_limits()
MAX_ROW_BYTES = IO_LIMITS["max_control_frame_bytes"]


def log(message):
    # The host caps a life's stderr for its whole lifetime, so this sensor
    # speaks only on transitions and stops speaking before the cap.
    global _LOG_LINES
    if _LOG_LINES >= MAX_LOG_LINES:
        return
    _LOG_LINES += 1
    text = "guardian_watch " + INTERFACE_ID + ": " + str(message)
    if _LOG_LINES == MAX_LOG_LINES:
        text = "guardian_watch " + INTERFACE_ID + ": log budget reached; further lines are not written"
    sys.stderr.write(text[:MAX_LOG_LINE_CHARS] + "\n")
    sys.stderr.flush()


def now_ms():
    return time.time_ns() // 1000000


def iso(unix_ms):
    if unix_ms is None:
        return None
    return datetime.datetime.fromtimestamp(unix_ms / 1000.0, datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def private_directory(path):
    os.makedirs(path, mode=0o700, exist_ok=True)
    state = os.lstat(path)
    if stat.S_ISLNK(state.st_mode) or not stat.S_ISDIR(state.st_mode):
        raise PermissionError("private path is not a directory: " + path)
    if stat.S_IMODE(state.st_mode) != 0o700:
        os.chmod(path, 0o700)
    return path


def read_bounded(path, maximum, name):
    before = os.lstat(path)
    if stat.S_ISLNK(before.st_mode) or not stat.S_ISREG(before.st_mode):
        raise PermissionError(name + " is not a regular file: " + path)
    if before.st_size > maximum:
        raise ValueError(name + " exceeds " + str(maximum) + " bytes: " + path)
    descriptor = os.open(path, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0))
    with os.fdopen(descriptor, "rb") as handle:
        value = handle.read(maximum + 1)
    if len(value) > maximum:
        raise ValueError(name + " grew beyond " + str(maximum) + " bytes: " + path)
    return value


def atomic_bytes(path, value):
    private_directory(os.path.dirname(path))
    temporary = path + "." + str(os.getpid()) + "." + str(time.time_ns()) + ".tmp"
    descriptor = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_NOFOLLOW", 0), 0o600)
    try:
        with os.fdopen(descriptor, "wb") as handle:
            descriptor = -1
            handle.write(value)
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(temporary, path)
    finally:
        if descriptor >= 0:
            os.close(descriptor)
        try:
            os.unlink(temporary)
        except FileNotFoundError:
            pass


def canonical_json(value):
    return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode("utf-8")


def config_path():
    # The registry folder holds the package's template and is rewritten by a
    # package repair or a rebuild; the private var directory survives both.
    # Only an absent private file selects the registry folder: a private file
    # that cannot be used is an error, never a fallback to the template.
    return PRIVATE_CONFIG_PATH if os.path.lexists(PRIVATE_CONFIG_PATH) else REGISTRY_CONFIG_PATH


def config_stamp():
    path = config_path()
    info = os.lstat(path)
    return (path, info.st_dev, info.st_ino, info.st_size, info.st_mtime_ns)


def load_config():
    stamp = config_stamp()
    raw = json.loads(read_bounded(stamp[0], MAX_CONFIG_BYTES, "guardian_config.json"))
    if not isinstance(raw, dict):
        raise ValueError("guardian_config.json must be a JSON object")
    required = {
        "schema_version", "watched_run", "provider_interface", "quiet_seconds", "repeat_cooldown_seconds",
        "max_repeat_cooldown_seconds", "refused_retry_seconds", "poll_seconds", "own_action_labels",
        "error_labels", "heartbeat_labels", "notice_label", "instructions",
    }
    missing = sorted(required - set(raw))
    unknown = sorted(set(raw) - required)
    if missing or unknown:
        raise ValueError("guardian_config.json keys are wrong; missing=" + str(missing) + " unknown=" + str(unknown))
    if raw["schema_version"] != 1:
        raise ValueError("guardian_config.json schema_version is unsupported")
    for key in ("watched_run", "provider_interface", "notice_label"):
        value = raw[key]
        if not isinstance(value, str) or not value or value.strip() != value or len(value) > 256:
            raise ValueError("guardian_config.json " + key + " must be a non-empty trimmed string")
    for key in ("quiet_seconds", "repeat_cooldown_seconds", "max_repeat_cooldown_seconds", "refused_retry_seconds", "poll_seconds"):
        value = raw[key]
        if isinstance(value, bool) or not isinstance(value, (int, float)) or not 0 < value <= 31536000:
            raise ValueError("guardian_config.json " + key + " must be a positive number of seconds")
    if raw["max_repeat_cooldown_seconds"] < raw["repeat_cooldown_seconds"]:
        raise ValueError("guardian_config.json max_repeat_cooldown_seconds is below repeat_cooldown_seconds")
    for key in ("own_action_labels", "error_labels", "heartbeat_labels"):
        value = raw[key]
        if not isinstance(value, list) or any(not isinstance(item, str) or not item for item in value):
            raise ValueError("guardian_config.json " + key + " must be a list of labels")
    if not raw["own_action_labels"]:
        raise ValueError("guardian_config.json own_action_labels must not be empty")
    if not isinstance(raw["instructions"], str):
        raise ValueError("guardian_config.json instructions must be a string")
    for key, placeholder in sorted(TEMPLATE_VALUES.items()):
        if raw[key] == placeholder:
            raise ValueError(
                "guardian_config.json still names the template " + key + " '" + placeholder
                + "'; write the installation's own guardian_config.json into the private var directory"
            )
    return raw, stamp


def fresh_inbound():
    return {"inputs": 0, "tool_results": 0, "recent": []}


def reset_observations(state):
    state["cursor"] = {"device": None, "inode": None, "offset": 0, "next_seq": 0}
    state["last_own_action"] = None
    state["last_provider_emission"] = None
    state["last_provider_error"] = None
    state["last_heartbeat"] = None
    state["inbound_since"] = fresh_inbound()
    state["ledger_tail"] = None


def empty_state(config):
    state = {
        "schema_version": 1,
        "watch": {"run": config["watched_run"], "provider": config["provider_interface"]},
        "started_at_unix_ms": now_ms(),
        "skipped_rows": 0,
        "alerts_sent": 0,
        "alert": None,
    }
    reset_observations(state)
    return state


def load_state(config):
    try:
        value = json.loads(read_bounded(STATE_PATH, MAX_ROW_BYTES, "guardian state"))
    except FileNotFoundError:
        return empty_state(config)
    except (OSError, ValueError) as error:
        log("the saved state is unreadable (" + str(error) + "); the ledger is folded again from its start")
        return empty_state(config)
    expected = set(empty_state(config))
    watch = {"run": config["watched_run"], "provider": config["provider_interface"]}
    if not isinstance(value, dict) or set(value) != expected or value.get("schema_version") != 1:
        log("the saved state has another shape; the ledger is folded again from its start")
        return empty_state(config)
    if value.get("watch") != watch:
        log("the watched run or provider changed; the ledger is folded again from its start")
        return empty_state(config)
    return value


def save_state(state):
    body = canonical_json(state) + b"\n"
    if len(body) > MAX_ROW_BYTES:
        raise ValueError("guardian state exceeds its size limit")
    atomic_bytes(STATE_PATH, body)


def endpoint(value):
    if isinstance(value, dict) and set(value) == {"Interface"} and isinstance(value["Interface"], str):
        return value["Interface"]
    return None


def label_of(movement):
    artifact = movement.get("artifact")
    identifier = artifact.get("id") if isinstance(artifact, dict) else None
    if isinstance(identifier, str) and ":" in identifier:
        return identifier.rsplit(":", 1)[0]
    return None


def run_of(movement):
    metadata = movement.get("invocation_metadata")
    runtime = metadata.get("runtime") if isinstance(metadata, dict) else None
    run = runtime.get("run") if isinstance(runtime, dict) else None
    return run if isinstance(run, str) else None


def observe(state, config, movement):
    sequence = movement.get("seq")
    recorded = movement.get("recorded_at_unix_ms")
    if type(sequence) is not int or type(recorded) is not int:
        return
    state["ledger_tail"] = {"seq": sequence, "recorded_at_unix_ms": recorded}
    kind = movement.get("kind")
    if kind not in ("InterfaceArrival", "InterfaceOutbound"):
        return
    if run_of(movement) != config["watched_run"]:
        return
    label = label_of(movement)
    if label is None:
        return
    source = endpoint(movement.get("from"))
    target = endpoint(movement.get("to"))
    provider = config["provider_interface"]
    entry = {"seq": sequence, "kind": kind, "label": label, "recorded_at_unix_ms": recorded}
    if source == provider:
        # The provider interface speaks for the model of this run: this is the run acting.
        state["last_provider_emission"] = entry
        if label in config["own_action_labels"]:
            state["last_own_action"] = entry
            state["inbound_since"] = fresh_inbound()
        elif label in config["error_labels"]:
            state["last_provider_error"] = entry
        return
    if kind == "InterfaceOutbound" and target == provider and source is not None:
        # Something was delivered to the run. It is evidence of demand, never of life.
        entry["from"] = source
        inbound = state["inbound_since"]
        if label in config["heartbeat_labels"]:
            state["last_heartbeat"] = entry
        if label.endswith(".result"):
            inbound["tool_results"] += 1
        else:
            inbound["inputs"] += 1
        inbound["recent"] = (inbound["recent"] + [entry])[-RECENT_LIMIT:]


def scan(state, config):
    try:
        descriptor = os.open(MOVEMENTS_JSONL, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0))
    except FileNotFoundError:
        return
    with os.fdopen(descriptor, "rb") as handle:
        info = os.fstat(handle.fileno())
        if not stat.S_ISREG(info.st_mode):
            raise PermissionError("the movement ledger is not a regular file")
        cursor = state["cursor"]
        if [cursor["device"], cursor["inode"]] != [info.st_dev, info.st_ino] or info.st_size < cursor["offset"]:
            if cursor["device"] is not None:
                log("the movement ledger changed identity or shrank; it is folded again from its start")
            reset_observations(state)
            cursor = state["cursor"]
            cursor["device"], cursor["inode"] = info.st_dev, info.st_ino
        handle.seek(cursor["offset"])
        while True:
            line = handle.readline(MAX_ROW_BYTES + 1)
            if len(line) > MAX_ROW_BYTES:
                raise RuntimeError("a ledger row at byte " + str(cursor["offset"]) + " exceeds the row limit")
            if not line or not line.endswith(b"\n"):
                return
            cursor["offset"] = handle.tell()
            try:
                movement = json.loads(line)
            except (UnicodeDecodeError, ValueError):
                movement = None
            if not isinstance(movement, dict):
                state["skipped_rows"] += 1
                log("a complete ledger row before byte " + str(cursor["offset"]) + " is not a JSON object and is passed over")
                continue
            if type(movement.get("seq")) is int:
                cursor["next_seq"] = movement["seq"] + 1
            observe(state, config, movement)


def repeat_wait_seconds(config, alert):
    if alert["status"] == "refused":
        return config["refused_retry_seconds"]
    doubled = config["repeat_cooldown_seconds"] * (2 ** min(alert["repeat_index"], 16))
    return min(doubled, config["max_repeat_cooldown_seconds"])


def evaluate(state, config, current_ms):
    own = state["last_own_action"]
    base_ms = own["recorded_at_unix_ms"] if own else state["started_at_unix_ms"]
    quiet_ms = current_ms - base_ms
    if quiet_ms <= config["quiet_seconds"] * 1000:
        return None
    key = own["seq"] if own else -1
    alert = state["alert"]
    if alert is None or alert["for_seq"] != key:
        return {"for_seq": key, "repeat_index": 0, "quiet_ms": quiet_ms}
    if alert["status"] == "prepared":
        return None
    if current_ms - alert["at_unix_ms"] < repeat_wait_seconds(config, alert) * 1000:
        return None
    step = 0 if alert["status"] == "refused" else 1
    return {"for_seq": key, "repeat_index": alert["repeat_index"] + step, "quiet_ms": quiet_ms}


def run_record_snapshot(config):
    try:
        slots = json.loads(os.environ.get("OCTOPUS_ROOT_SLOTS_JSON", "{}"))
        root = slots.get("run_records_root") if isinstance(slots, dict) else None
        if not isinstance(root, str) or not os.path.isabs(root):
            return {"readable": False, "reason": "run_records_root is not granted to this sensor"}
        path = os.path.join(root, config["watched_run"] + ".json")
        info = os.stat(path)
        record = json.loads(read_bounded(path, MAX_RUN_RECORD_BYTES, "run record"))
        if not isinstance(record, dict):
            return {"readable": False, "reason": "the run record is not a JSON object"}
        active = record.get("active_checkpoints")
        checkpoints = record.get("checkpoints")
        return {
            "readable": True,
            "path": path,
            "status": record.get("status") if isinstance(record.get("status"), str) else None,
            "ee": record.get("ee") if isinstance(record.get("ee"), str) else None,
            "active_checkpoints": len(active) if isinstance(active, list) else None,
            "checkpoints": len(checkpoints) if isinstance(checkpoints, list) else None,
            "size_bytes": info.st_size,
            "modified_at": iso(info.st_mtime_ns // 1000000),
        }
    except (OSError, ValueError) as error:
        return {"readable": False, "reason": str(error)[:300]}


def with_time(entry):
    if entry is None:
        return None
    described = dict(entry)
    described["recorded_at"] = iso(entry["recorded_at_unix_ms"])
    return described


def build_payload(state, config, due, current_ms):
    own = state["last_own_action"]
    inbound = state["inbound_since"]
    heartbeat = state["last_heartbeat"]
    tail = state["ledger_tail"]
    quiet_seconds = due["quiet_ms"] // 1000
    since = ("since " + iso(own["recorded_at_unix_ms"]) + " (ledger seq " + str(own["seq"]) + ", " + own["label"] + ")") if own \
        else ("since this sensor first started at " + iso(state["started_at_unix_ms"]) + "; no own action was ever seen")
    event = (
        "The run " + config["watched_run"] + " has emitted nothing of its own through "
        + config["provider_interface"] + " for " + str(quiet_seconds // 60) + " minutes, " + since
        + ". Deliveries to it do not count. This is report " + str(due["repeat_index"] + 1) + " of this silence."
    )
    next_alert = {"status": "admitted", "repeat_index": due["repeat_index"]}
    return {
        "source": "guardian_watch",
        "source_interface": INTERFACE_ID,
        "trigger": {
            "event": event,
            "watched_run": config["watched_run"],
            "provider_interface": config["provider_interface"],
            "quiet_threshold_seconds": config["quiet_seconds"],
            "own_action_labels": config["own_action_labels"],
        },
        "observed_at": iso(current_ms),
        "observed_at_unix_ms": current_ms,
        "quiet_seconds": quiet_seconds,
        "last_own_action": with_time(own),
        "last_provider_emission": with_time(state["last_provider_emission"]),
        "last_provider_error": with_time(state["last_provider_error"]),
        "delivered_since_last_own_action": {
            "inputs": inbound["inputs"],
            "tool_results": inbound["tool_results"],
            "recent": [with_time(entry) for entry in inbound["recent"]],
        },
        "last_heartbeat_delivery": with_time(heartbeat),
        "heartbeat_delivered_since_last_own_action": bool(heartbeat and (own is None or heartbeat["seq"] > own["seq"])),
        "run_record": run_record_snapshot(config),
        "ledger": {
            "last_seq": tail["seq"] if tail else None,
            "last_recorded_at": iso(tail["recorded_at_unix_ms"]) if tail else None,
            "seconds_since_last_row": (current_ms - tail["recorded_at_unix_ms"]) // 1000 if tail else None,
            "unreadable_rows_passed_over": state["skipped_rows"],
        },
        "alert": {
            "number": state["alerts_sent"] + 1,
            "repeat_index": due["repeat_index"],
            "next_report_of_this_silence_after_seconds": repeat_wait_seconds(config, next_alert),
        },
        "task": config["instructions"],
    }


def prepare_alert(state, config, due, current_ms):
    payload = canonical_json(build_payload(state, config, due, current_ms))
    if len(payload) > IO_LIMITS["max_payload_bytes"]:
        raise ValueError("the alert payload exceeds max_payload_bytes")
    private_directory(PAYLOADS_DIR)
    for name in os.listdir(PAYLOADS_DIR):
        # Earlier alerts are already in the blob store or were refused; only one is ever pending.
        try:
            os.unlink(os.path.join(PAYLOADS_DIR, name))
        except OSError:
            pass
    payload_hash = octopus_content_hash(payload)
    payload_path = os.path.join(PAYLOADS_DIR, "alert-" + payload_hash + ".payload")
    atomic_bytes(payload_path, payload)
    manifest = canonical_json({"label": config["notice_label"], "payload_path": payload_path, "payload_content_hash": payload_hash})
    if len(manifest) > IO_LIMITS["max_manifest_bytes"]:
        raise ValueError("the alert manifest exceeds max_manifest_bytes")
    manifest_hash = octopus_content_hash(manifest)
    manifest_path = os.path.join(PAYLOADS_DIR, "alert-" + manifest_hash + ".artifact.json")
    atomic_bytes(manifest_path, manifest)
    state["alert"] = {
        "status": "prepared",
        "for_seq": due["for_seq"],
        "repeat_index": due["repeat_index"],
        "at_unix_ms": current_ms,
        "label": config["notice_label"],
        "manifest_path": manifest_path,
        "manifest_hash": manifest_hash,
        "ledger_offset": state["cursor"]["offset"],
        "admitted_seq": None,
        "refusal": None,
    }
    save_state(state)
    log("silent for " + str(due["quiet_ms"] // 1000) + "s; handing " + config["notice_label"] + " (report " + str(due["repeat_index"] + 1) + " of this silence)")


class Refused(RuntimeError):
    def __init__(self, verdict):
        refused = verdict.get("refused") if isinstance(verdict.get("refused"), dict) else {}
        self.reason = refused.get("reason") if isinstance(refused.get("reason"), str) else None
        self.detail = refused.get("detail") if isinstance(refused.get("detail"), dict) else {}
        super().__init__("the runtime refused the alert (" + str(self.reason) + "): " + json.dumps(self.detail, sort_keys=True)[:400])

    def transient(self):
        return self.reason == "registry_unstable" or (
            self.reason == "stale_epoch" and self.detail.get("reason_detail") == "arrival_target_changed"
        )

    def duplicate_of(self, artifact_id):
        return self.detail.get("artifact_id") == artifact_id and self.detail.get("message") == DUPLICATE_MESSAGE

    def record(self):
        return {"reason": self.reason, "detail": json.dumps(self.detail, sort_keys=True)[:2000]}


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
        raise RuntimeError("runtime verdict input closed before the alert was admitted")
    if len(line) > IO_LIMITS["max_control_frame_bytes"] or not line.endswith(b"\n"):
        raise RuntimeError("runtime verdict is oversized or partial")
    response = json.loads(line)
    if not isinstance(response, dict) or response.get("kind") != "delivery_verdict" or response.get("request_id") != request_id:
        raise RuntimeError("runtime verdict does not match the pending alert")
    verdict = response.get("verdict")
    if isinstance(verdict, dict) and set(verdict) == {"admitted"}:
        admitted = verdict["admitted"]
        if isinstance(admitted, dict) and type(admitted.get("seq")) is int and admitted["seq"] >= 0:
            return admitted["seq"]
    if isinstance(verdict, dict) and set(verdict) == {"refused"}:
        raise Refused(verdict)
    raise RuntimeError("runtime delivery verdict has an invalid shape")


def find_admitted(artifact_id, start_offset):
    needle = artifact_id.encode("utf-8")
    try:
        handle = open(MOVEMENTS_JSONL, "rb")
    except FileNotFoundError:
        return None
    with handle:
        size = os.fstat(handle.fileno()).st_size
        handle.seek(start_offset if isinstance(start_offset, int) and 0 <= start_offset <= size else 0)
        while True:
            line = handle.readline(MAX_ROW_BYTES + 1)
            if not line or not line.endswith(b"\n"):
                return None
            if needle not in line:
                continue
            try:
                movement = json.loads(line)
            except (UnicodeDecodeError, ValueError):
                continue
            if (
                isinstance(movement, dict)
                and movement.get("kind") == "InterfaceArrival"
                and movement.get("from") == {"Interface": INTERFACE_ID}
                and isinstance(movement.get("artifact"), dict)
                and movement["artifact"].get("id") == artifact_id
                and type(movement.get("seq")) is int
            ):
                return movement["seq"]


def deliver(state):
    alert = state["alert"]
    artifact_id = alert["label"] + ":" + alert["manifest_hash"]
    evidence = [{
        "interface_id": INTERFACE_ID,
        "artifact_id": artifact_id,
        "label": EVIDENCE_LABEL,
        "bytes": list(bytes.fromhex(octopus_content_hash(artifact_id.encode("utf-8")))),
    }]
    admitted = find_admitted(artifact_id, alert.get("ledger_offset"))
    delay = 1
    attempts = 0
    while admitted is None:
        try:
            admitted = hand(alert["manifest_path"], evidence)
        except Refused as refusal:
            if refusal.duplicate_of(artifact_id):
                admitted = find_admitted(artifact_id, alert.get("ledger_offset"))
                if admitted is None:
                    raise RuntimeError("the runtime reports the alert as already handed, but the ledger has no such arrival")
            elif refusal.transient() and attempts < TRANSIENT_ATTEMPTS:
                attempts += 1
                time.sleep(delay)
                delay = min(delay * 2, 30)
            else:
                alert["status"] = "refused"
                alert["refusal"] = refusal.record()
                save_state(state)
                log(str(refusal) + "; the same silence is reported again after the refused-retry wait")
                return
    alert["status"] = "admitted"
    alert["admitted_seq"] = admitted
    state["alerts_sent"] += 1
    save_state(state)
    log("the alert " + artifact_id + " was admitted at ledger seq " + str(admitted))


def describe(state, config):
    own = state["last_own_action"]
    last = ("last own action at seq " + str(own["seq"]) + " (" + own["label"] + ", " + iso(own["recorded_at_unix_ms"]) + ")") if own else "no own action seen yet"
    return (
        "up: watching " + config["watched_run"] + " through " + config["provider_interface"]
        + "; alert after " + str(config["quiet_seconds"]) + "s of silence; " + last
        + "; ledger cursor at seq " + str(state["cursor"]["next_seq"])
    )


def main():
    os.umask(0o077)
    private_directory(VAR_DIR)
    private_directory(PAYLOADS_DIR)
    config, stamp = load_config()
    state = load_state(config)
    scan(state, config)
    save_state(state)
    log(describe(state, config))
    write_frame({"kind": "ready"})
    if state["alert"] is not None and state["alert"]["status"] == "prepared":
        log("handing the alert prepared before the last stop again")
        deliver(state)
    while True:
        try:
            current_stamp = config_stamp()
        except OSError:
            current_stamp = stamp
        if current_stamp != stamp:
            try:
                config, stamp = load_config()
                state = load_state(config)
                log("guardian_config.json changed and was read again")
            except (OSError, ValueError) as error:
                stamp = current_stamp
                log("guardian_config.json changed but is not usable (" + str(error) + "); the previous configuration stays in force")
        offset = state["cursor"]["offset"]
        scan(state, config)
        current_ms = now_ms()
        due = evaluate(state, config, current_ms)
        if due is not None:
            prepare_alert(state, config, due, current_ms)
            deliver(state)
        elif state["cursor"]["offset"] != offset:
            save_state(state)
        time.sleep(config["poll_seconds"])


if __name__ == "__main__":
    try:
        main()
    except Exception as error:
        log("stopped: " + str(error))
        raise SystemExit(1)
