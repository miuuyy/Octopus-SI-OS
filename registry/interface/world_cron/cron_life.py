#!/usr/bin/env python3
import importlib.util
import json
import hashlib
import math
import os
import re
import signal
import stat
import sys
import time
from datetime import datetime, timedelta, timezone
from zoneinfo import ZoneInfo, ZoneInfoNotFoundError

_HASH_MODULE_PATH = os.path.join(
    os.path.dirname(os.path.abspath(__file__)),
    "octopus_content_hash.py",
)
_HASH_MODULE_SPEC = importlib.util.spec_from_file_location(
    "octopus_world_cron_content_hash",
    _HASH_MODULE_PATH,
)
if _HASH_MODULE_SPEC is None or _HASH_MODULE_SPEC.loader is None:
    raise RuntimeError("cron content-hash dependency cannot be loaded")
_HASH_MODULE = importlib.util.module_from_spec(_HASH_MODULE_SPEC)
_HASH_MODULE_SPEC.loader.exec_module(_HASH_MODULE)
octopus_content_hash = _HASH_MODULE.octopus_content_hash

INTERFACE_ID = os.environ["OCTOPUS_INTERFACE_ID"]
INTERFACE_DIR = os.path.abspath(os.environ["OCTOPUS_INTERFACE_DIR"])
VAR_DIR = os.path.abspath(os.environ["OCTOPUS_VAR_DIR"])
INTERFACE_DESCRIPTOR_JSON = os.environ["OCTOPUS_INTERFACE_DESCRIPTOR_JSON"]

ACTIVATION_PATH = os.path.join(INTERFACE_DIR, "activation.json")
PAYLOADS_DIR = os.path.join(VAR_DIR, "payloads")
STATE_PATH = os.path.join(VAR_DIR, "cron-state.json")
START_STATE_PATH = os.path.join(VAR_DIR, "cron-start-state.json")
INTERVAL_STATE_PATH = os.path.join(VAR_DIR, "cron-interval-state.json")
INACTIVITY_STATE_PATH = os.path.join(VAR_DIR, "cron-inactivity-state.json")
ENV_NAME = re.compile(r"[A-Z_][A-Z0-9_]*\Z")
REGISTRY_ID = re.compile(r"[a-z0-9][a-z0-9_-]*\Z")
TIME_LABEL = "cron.current_time"

_NEXT_REQUEST_ID = 1
_TEMP_SEQUENCE = 1


class DeliveryRefused(RuntimeError):
    def __init__(self, verdict):
        super().__init__(f"runtime refused cron artifact delivery: {verdict!r}")
        self.verdict = verdict


def load_host_io_limits():
    try:
        value = json.loads(os.environ["OCTOPUS_INTERFACE_HOST_IO_LIMITS"])
    except (KeyError, json.JSONDecodeError) as error:
        raise RuntimeError("cron host IO limits are absent or malformed") from error
    expected = {
        "max_control_frame_bytes",
        "max_manifest_bytes",
        "max_payload_bytes",
        "max_stderr_bytes",
    }
    if not isinstance(value, dict) or set(value) != expected:
        raise RuntimeError("cron host IO limits have an invalid shape")
    for key in expected:
        limit = value[key]
        if isinstance(limit, bool) or not isinstance(limit, int) or limit <= 0:
            raise RuntimeError(f"cron host IO limit {key} must be a positive integer")
    return value


HOST_IO_LIMITS = load_host_io_limits()
MAX_AUTHORING_MANIFEST_BYTES = HOST_IO_LIMITS["max_manifest_bytes"]
MAX_RUN_RECORD_BYTES = HOST_IO_LIMITS["max_payload_bytes"]
MAX_LEDGER_ROW_BYTES = HOST_IO_LIMITS["max_control_frame_bytes"]


def private_directory(path):
    os.makedirs(path, mode=0o700, exist_ok=True)
    state = os.lstat(path)
    if stat.S_ISLNK(state.st_mode) or not stat.S_ISDIR(state.st_mode):
        raise PermissionError(f"cron private path is not a directory: {path}")
    if hasattr(os, "geteuid") and state.st_uid != os.geteuid():
        raise PermissionError(f"cron private path has the wrong owner: {path}")
    if stat.S_IMODE(state.st_mode) != 0o700:
        os.chmod(path, 0o700)
    return path


def load_json_object(path, label, maximum_bytes, private=False):
    body = read_regular_file_bounded(path, maximum_bytes, label, private=private)
    try:
        value = json.loads(body)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise ValueError(f"{label} is malformed JSON") from error
    if not isinstance(value, dict):
        raise ValueError(f"{label} must be a JSON object")
    return value


class FileChangedDuringRead(RuntimeError):
    pass


def read_regular_file_bounded(path, maximum_bytes, label, private=False):
    linked_before = os.lstat(path)
    if stat.S_ISLNK(linked_before.st_mode) or not stat.S_ISREG(linked_before.st_mode):
        raise RuntimeError(f"{label} is not a regular file")
    if linked_before.st_size > maximum_bytes:
        raise RuntimeError(f"{label} exceeds {maximum_bytes} bytes")
    if private:
        if hasattr(os, "geteuid") and linked_before.st_uid != os.geteuid():
            raise PermissionError(f"{label} has the wrong owner")
        if stat.S_IMODE(linked_before.st_mode) != 0o600:
            os.chmod(path, 0o600)
            linked_before = os.lstat(path)
            if stat.S_IMODE(linked_before.st_mode) != 0o600:
                raise PermissionError(f"{label} must have mode 0600")
    flags = os.O_RDONLY | getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_NOFOLLOW", 0)
    descriptor = os.open(path, flags)
    try:
        before = os.fstat(descriptor)
        linked_open = os.lstat(path)
        if (
            not stat.S_ISREG(before.st_mode)
            or stat.S_ISLNK(linked_open.st_mode)
            or (before.st_dev, before.st_ino)
            != (linked_before.st_dev, linked_before.st_ino)
            or (linked_open.st_dev, linked_open.st_ino)
            != (before.st_dev, before.st_ino)
        ):
            raise FileChangedDuringRead(f"{label} changed while it was opened")
        chunks = []
        observed = 0
        while observed <= maximum_bytes:
            chunk = os.read(descriptor, min(64 * 1024, maximum_bytes + 1 - observed))
            if not chunk:
                break
            chunks.append(chunk)
            observed += len(chunk)
        if observed > maximum_bytes:
            raise RuntimeError(f"{label} exceeds {maximum_bytes} bytes")
        after = os.fstat(descriptor)
        linked_after = os.lstat(path)
        before_identity = (
            before.st_dev,
            before.st_ino,
            before.st_size,
            before.st_mtime_ns,
            before.st_ctime_ns,
        )
        after_identity = (
            after.st_dev,
            after.st_ino,
            after.st_size,
            after.st_mtime_ns,
            after.st_ctime_ns,
        )
        if (
            before_identity != after_identity
            or observed != after.st_size
            or stat.S_ISLNK(linked_after.st_mode)
            or (linked_after.st_dev, linked_after.st_ino)
            != (after.st_dev, after.st_ino)
        ):
            raise FileChangedDuringRead(f"{label} changed while it was read")
        return b"".join(chunks)
    finally:
        os.close(descriptor)


def resolve_timezone(timezone_name):
    if timezone_name == "UTC":
        return timezone.utc
    if timezone_name == "local":
        return datetime.now().astimezone().tzinfo
    segments = timezone_name.split("/")
    if any(segment in {"", ".", ".."} for segment in segments):
        raise ZoneInfoNotFoundError(timezone_name)
    timezone_path = os.path.join(
        root_slots()["timezone_database_root"],
        *segments,
    )
    try:
        with open(timezone_path, "rb") as handle:
            return ZoneInfo.from_file(handle, key=timezone_name)
    except (OSError, ValueError) as error:
        raise ZoneInfoNotFoundError(timezone_name) from error


def sync_directory(path):
    descriptor = os.open(path, os.O_RDONLY | getattr(os, "O_CLOEXEC", 0))
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def atomic_write_bytes(path, body):
    global _TEMP_SEQUENCE
    parent = private_directory(os.path.dirname(path))
    temporary = os.path.join(
        parent,
        f".{os.path.basename(path)}.{os.getpid()}.{_TEMP_SEQUENCE}.tmp",
    )
    _TEMP_SEQUENCE += 1
    descriptor = os.open(
        temporary,
        os.O_WRONLY
        | os.O_CREAT
        | os.O_EXCL
        | getattr(os, "O_CLOEXEC", 0)
        | getattr(os, "O_NOFOLLOW", 0),
        0o600,
    )
    try:
        with os.fdopen(descriptor, "wb") as handle:
            descriptor = -1
            handle.write(body)
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(temporary, path)
        os.chmod(path, 0o600)
        sync_directory(parent)
    finally:
        if descriptor >= 0:
            os.close(descriptor)
        try:
            os.unlink(temporary)
        except FileNotFoundError:
            pass


def exact_keys(value, allowed, required, label):
    if not isinstance(value, dict):
        raise ValueError(f"{label} must be a JSON object")
    unknown = sorted(set(value) - set(allowed))
    missing = sorted(set(required) - set(value))
    if unknown:
        raise ValueError(f"{label} contains unknown fields: {unknown!r}")
    if missing:
        raise ValueError(f"{label} is missing required fields: {missing!r}")


def root_slots():
    try:
        value = json.loads(os.environ["OCTOPUS_ROOT_SLOTS_JSON"])
    except (KeyError, json.JSONDecodeError) as error:
        raise RuntimeError("OCTOPUS_ROOT_SLOTS_JSON is absent or malformed") from error
    if not isinstance(value, dict):
        raise RuntimeError("OCTOPUS_ROOT_SLOTS_JSON must be a JSON object")
    for name in (
        "run_records_root",
        "timezone_database_root",
    ):
        path = value.get(name)
        if not isinstance(path, str) or not path or not os.path.isabs(path):
            raise RuntimeError(f"OCTOPUS_ROOT_SLOTS_JSON has no absolute {name!r}")
    return value


def load_schedule():
    descriptor = json.loads(INTERFACE_DESCRIPTOR_JSON)
    if not isinstance(descriptor, dict):
        raise ValueError("resolved interface descriptor must be a JSON object")
    if descriptor.get("id") != INTERFACE_ID:
        raise ValueError(
            f"interface descriptor id {descriptor.get('id')!r} does not match {INTERFACE_ID!r}"
        )
    driver_ref = descriptor.get("interface_driver_ref")
    if not isinstance(driver_ref, str) or not driver_ref:
        raise ValueError("interface descriptor has no interface_driver_ref")

    document = load_json_object(
        ACTIVATION_PATH,
        "activation document",
        HOST_IO_LIMITS["max_manifest_bytes"],
    )
    exact_keys(
        document,
        {"schema_version", "interface", "activations"},
        {"schema_version", "interface", "activations"},
        "activation document",
    )
    if document["schema_version"] != 1:
        raise ValueError(
            f"activation schema version {document['schema_version']!r} is unsupported"
        )
    if document["interface"] != INTERFACE_ID:
        raise ValueError(
            f"activation interface {document['interface']!r} does not match {INTERFACE_ID!r}"
        )
    activations = document["activations"]
    if not isinstance(activations, list) or not activations:
        raise ValueError("activation document must contain a non-empty activations array")

    selected = []
    allowed_activation_keys = {
        "activation_driver_ref",
        "enabled",
        "config",
    }
    for index, activation in enumerate(activations):
        if not isinstance(activation, dict):
            raise ValueError(f"activation #{index} must be a JSON object")
        exact_keys(
            activation,
            allowed_activation_keys,
            {"activation_driver_ref", "config"},
            f"activation #{index}",
        )
        enabled = activation.get("enabled", True)
        if not isinstance(enabled, bool):
            raise ValueError(f"activation #{index}.enabled must be a boolean")
        if enabled and activation["activation_driver_ref"] == driver_ref:
            selected.append(activation)
    if not selected and any(activation["activation_driver_ref"] == driver_ref for activation in activations):
        return None
    if len(selected) != 1:
        raise ValueError(
            f"interface {INTERFACE_ID!r} requires exactly one enabled {driver_ref!r} activation; found {len(selected)}"
        )

    activation = selected[0]
    config = activation["config"]
    if not isinstance(config, dict):
        raise ValueError("cron activation config must be a JSON object")
    exact_keys(config, {"schedule"}, {"schedule"}, "cron activation config")
    schedule = config["schedule"]
    if not isinstance(schedule, dict):
        raise ValueError("cron activation config.schedule must be a JSON object")
    exact_keys(
        schedule,
        {
            "interval_seconds",
            "cron",
            "due_at",
            "inactivity",
            "fire_on_start",
            "timezone",
            "reminder_env",
            "reminder_default",
        },
        {"fire_on_start", "timezone", "reminder_env", "reminder_default"},
        "cron activation config.schedule",
    )

    fire_on_start = schedule["fire_on_start"]
    if not isinstance(fire_on_start, bool):
        raise ValueError("schedule.fire_on_start must be a boolean")
    reminder_env = schedule["reminder_env"]
    if not isinstance(reminder_env, str):
        raise ValueError("schedule.reminder_env must be a string")
    if reminder_env and ENV_NAME.fullmatch(reminder_env) is None:
        raise ValueError(f"schedule.reminder_env is invalid: {reminder_env!r}")
    reminder_default = schedule["reminder_default"]
    if not isinstance(reminder_default, str):
        raise ValueError("schedule.reminder_default must be a string")

    interval = schedule.get("interval_seconds")
    cron = schedule.get("cron")
    due_at = schedule.get("due_at")
    inactivity = schedule.get("inactivity")
    has_interval = interval is not None
    has_cron = cron is not None
    has_due_at = due_at is not None
    if sum((has_interval, has_cron, has_due_at, inactivity is not None)) != 1:
        raise ValueError("schedule must declare exactly one of interval_seconds, cron, due_at or inactivity")
    if inactivity is not None:
        validate_inactivity(inactivity)
        if fire_on_start:
            raise ValueError("inactivity schedule requires fire_on_start=false")
    due_unix_ns = parse_due_at(due_at) if has_due_at else None
    if has_due_at and fire_on_start:
        raise ValueError("one-shot schedule.due_at requires fire_on_start=false")
    if has_interval:
        if isinstance(interval, bool) or not isinstance(interval, (int, float)):
            raise ValueError("schedule.interval_seconds must be a number")
        if not math.isfinite(interval) or interval <= 0:
            raise ValueError("schedule.interval_seconds must be finite and greater than zero")
        interval_ns = int(round(float(interval) * 1_000_000_000))
        if interval_ns <= 0:
            raise ValueError("schedule.interval_seconds is below nanosecond resolution")
    else:
        if has_cron and (not isinstance(cron, str) or not cron.strip()):
            raise ValueError("schedule.cron must be a non-empty string")
        interval_ns = None

    timezone_name = schedule["timezone"]
    if not isinstance(timezone_name, str) or not timezone_name:
        raise ValueError("schedule.timezone must be a non-empty string")
    try:
        timezone_value = resolve_timezone(timezone_name)
    except ZoneInfoNotFoundError as error:
        raise ValueError(f"schedule.timezone is unknown: {timezone_name!r}") from error

    return {
        "interval_seconds": interval,
        "interval_ns": interval_ns,
        "cron": cron,
        "due_at": due_at,
        "due_unix_ns": due_unix_ns,
        "inactivity": inactivity,
        "fire_on_start": fire_on_start,
        "timezone": timezone_value,
        "timezone_name": timezone_name,
        "reminder_env": reminder_env,
        "reminder_default": reminder_default,
        "fingerprint": hashlib.sha256(
            json.dumps(
                {
                    "interface": INTERFACE_ID,
                    "interval_seconds": interval,
                    "cron": cron,
                    "fire_on_start": fire_on_start,
                    "timezone": timezone_name,
                    "reminder_env": reminder_env,
                    "reminder_default": reminder_default,
                    **({"due_unix_ns": due_unix_ns} if has_due_at else {}),
                    **({"inactivity": inactivity} if inactivity is not None else {}),
                },
                separators=(",", ":"),
                sort_keys=True,
            ).encode("utf-8")
        ).hexdigest(),
    }


def parse_due_at(value):
    if not isinstance(value, str):
        raise ValueError("schedule.due_at must be an absolute RFC3339 timestamp")
    match = re.fullmatch(
        r"(\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2})(?:\.(\d{1,9}))?(Z|[+-]\d{2}:\d{2})",
        value,
    )
    if match is None or match[3] == "-00:00":
        raise ValueError("schedule.due_at requires an explicit known UTC offset")
    offset = match[3]
    if offset != "Z" and (int(offset[1:3]) > 23 or int(offset[4:]) > 59):
        raise ValueError("schedule.due_at has an invalid UTC offset")
    try:
        moment = datetime.fromisoformat(match[1] + ("+00:00" if offset == "Z" else offset))
        delta = moment.astimezone(timezone.utc) - datetime(1970, 1, 1, tzinfo=timezone.utc)
    except (ValueError, OverflowError) as error:
        raise ValueError("schedule.due_at is not a valid absolute timestamp") from error
    due = (delta.days * 86400 + delta.seconds) * 1_000_000_000 + int((match[2] or "0").ljust(9, "0"))
    if not 0 <= due <= 9_223_372_036_854_775_807:
        raise ValueError("schedule.due_at is outside the supported Unix nanosecond range")
    return due


def parse_integer(raw, label):
    if not raw or not raw.isascii() or not raw.isdigit():
        raise ValueError(f"{label} must be an unsigned integer; got {raw!r}")
    return int(raw)


def expand_cron_field(raw, minimum, maximum, label, sunday_alias=False):
    if not isinstance(raw, str) or not raw:
        raise ValueError(f"cron {label} field must be a non-empty string")
    values = set()
    accepted_maximum = 7 if sunday_alias else maximum
    for item in raw.split(","):
        if not item:
            raise ValueError(f"cron {label} contains an empty list item")
        pieces = item.split("/")
        if len(pieces) > 2:
            raise ValueError(f"cron {label} has an invalid step expression: {item!r}")
        base = pieces[0]
        step = parse_integer(pieces[1], f"cron {label} step") if len(pieces) == 2 else 1
        if step <= 0:
            raise ValueError(f"cron {label} step must be greater than zero")
        if base == "*":
            lower, upper = minimum, accepted_maximum
        elif "-" in base:
            bounds = base.split("-")
            if len(bounds) != 2:
                raise ValueError(f"cron {label} has an invalid range: {base!r}")
            lower = parse_integer(bounds[0], f"cron {label} range start")
            upper = parse_integer(bounds[1], f"cron {label} range end")
        else:
            lower = upper = parse_integer(base, f"cron {label} value")
        if lower < minimum or upper > accepted_maximum or lower > upper:
            raise ValueError(
                f"cron {label} range {lower}-{upper} is outside {minimum}-{accepted_maximum}"
            )
        for value in range(lower, upper + 1, step):
            values.add(0 if sunday_alias and value == 7 else value)
    if not values:
        raise ValueError(f"cron {label} selects no values")
    return frozenset(values)


def compile_cron(expression):
    fields = expression.split()
    if len(fields) != 5:
        raise ValueError(
            f"cron string must have five fields (minute hour day-of-month month day-of-week); got {expression!r}"
        )
    minute, hour, day_of_month, month, day_of_week = fields
    return {
        "minutes": expand_cron_field(minute, 0, 59, "minute"),
        "hours": expand_cron_field(hour, 0, 23, "hour"),
        "days_of_month": expand_cron_field(day_of_month, 1, 31, "day-of-month"),
        "months": expand_cron_field(month, 1, 12, "month"),
        "days_of_week": expand_cron_field(day_of_week, 0, 6, "day-of-week", True),
        "day_of_month_wildcard": day_of_month == "*",
        "day_of_week_wildcard": day_of_week == "*",
    }


def cron_matches(compiled, moment):
    if moment.minute not in compiled["minutes"]:
        return False
    if moment.hour not in compiled["hours"]:
        return False
    if moment.month not in compiled["months"]:
        return False
    day_of_month_match = moment.day in compiled["days_of_month"]
    cron_day_of_week = (moment.weekday() + 1) % 7
    day_of_week_match = cron_day_of_week in compiled["days_of_week"]
    if compiled["day_of_month_wildcard"]:
        return day_of_week_match
    if compiled["day_of_week_wildcard"]:
        return day_of_month_match
    return day_of_month_match or day_of_week_match


def next_cron_minute(compiled, moment):
    candidate = moment.replace(second=0, microsecond=0) + timedelta(minutes=1)
    limit = candidate + timedelta(days=366 * 8)
    while candidate < limit:
        if cron_matches(compiled, candidate):
            return candidate
        candidate += timedelta(minutes=1)
    raise ValueError("cron expression has no occurrence in the next eight years")


def atomic_write_json(path, value, maximum_bytes=None):
    body = canonical_json(value)
    limit = HOST_IO_LIMITS["max_control_frame_bytes"] if maximum_bytes is None else maximum_bytes
    if len(body) > limit:
        raise RuntimeError(f"cron private state exceeds its {limit}-byte limit")
    atomic_write_bytes(path, body)


def canonical_json(value):
    return json.dumps(value, separators=(",", ":"), sort_keys=True).encode("utf-8")


def load_last_fired_minute():
    try:
        state = load_json_object(
            STATE_PATH,
            "cron state",
            HOST_IO_LIMITS["max_control_frame_bytes"],
            private=True,
        )
    except FileNotFoundError:
        return None
    exact_keys(state, {"last_cron_minute"}, {"last_cron_minute"}, "cron state")
    value = state["last_cron_minute"]
    if not isinstance(value, str) or not value:
        raise ValueError("cron state last_cron_minute must be a non-empty string")
    return value


def commit_last_fired_minute(minute_key):
    atomic_write_json(STATE_PATH, {"last_cron_minute": minute_key})


def load_fire_on_start_state(schedule):
    try:
        state = load_json_object(
            START_STATE_PATH,
            "cron start state",
            HOST_IO_LIMITS["max_control_frame_bytes"],
            private=True,
        )
    except FileNotFoundError:
        return None
    exact_keys(
        state,
        {
            "schema_version",
            "schedule_fingerprint",
            "status",
            "artifact_label",
            "manifest_hash",
            "admitted_seq",
        },
        {
            "schema_version",
            "schedule_fingerprint",
            "status",
            "artifact_label",
            "manifest_hash",
            "admitted_seq",
        },
        "cron start state",
    )
    if state["schema_version"] != 1:
        raise ValueError("cron start state schema_version is unsupported")
    fingerprint = state["schedule_fingerprint"]
    if not isinstance(fingerprint, str) or not fingerprint:
        raise ValueError("cron start state schedule_fingerprint must be a non-empty string")
    if fingerprint != schedule["fingerprint"]:
        return None
    status_value = state["status"]
    if status_value == "completed_without_artifact":
        if any(
            state[field] is not None
            for field in ("artifact_label", "manifest_hash", "admitted_seq")
        ):
            raise ValueError("cron skipped start state carries artifact fields")
        return state
    if status_value not in {"prepared", "admitted"}:
        raise ValueError("cron start state has an invalid status")
    label = state["artifact_label"]
    manifest_hash = state["manifest_hash"]
    if not isinstance(label, str) or not label:
        raise ValueError("cron start state has no artifact label")
    if not isinstance(manifest_hash, str) or re.fullmatch(r"[0-9a-f]{64}", manifest_hash) is None:
        raise ValueError("cron start state has an invalid manifest hash")
    admitted_seq = state["admitted_seq"]
    if status_value == "prepared" and admitted_seq is not None:
        raise ValueError("cron prepared start state already carries admitted_seq")
    if status_value == "admitted" and (
        isinstance(admitted_seq, bool)
        or not isinstance(admitted_seq, int)
        or admitted_seq < 0
    ):
        raise ValueError("cron admitted start state has an invalid admitted_seq")
    return state


def fire_on_start_completed(schedule):
    state = load_fire_on_start_state(schedule)
    return state is not None and state["status"] in {
        "admitted",
        "completed_without_artifact",
    }


def commit_fire_on_start_state(schedule, status_value, label=None, manifest_hash=None, admitted_seq=None):
    atomic_write_json(
        START_STATE_PATH,
        {
            "schema_version": 1,
            "schedule_fingerprint": schedule["fingerprint"],
            "status": status_value,
            "artifact_label": label,
            "manifest_hash": manifest_hash,
            "admitted_seq": admitted_seq,
        },
    )


def load_interval_state(schedule):
    try:
        state = load_json_object(
            INTERVAL_STATE_PATH,
            "cron interval state",
            HOST_IO_LIMITS["max_control_frame_bytes"],
            private=True,
        )
    except FileNotFoundError:
        return None
    exact_keys(
        state,
        {
            "schema_version",
            "schedule_fingerprint",
            "status",
            "next_due_unix_ns",
            "artifact_label",
            "manifest_hash",
        },
        {
            "schema_version",
            "schedule_fingerprint",
            "status",
            "next_due_unix_ns",
            "artifact_label",
            "manifest_hash",
        },
        "cron interval state",
    )
    if state["schema_version"] != 1:
        raise ValueError("cron interval state schema_version is unsupported")
    fingerprint = state["schedule_fingerprint"]
    if not isinstance(fingerprint, str) or not fingerprint:
        raise ValueError("cron interval state schedule_fingerprint must be non-empty")
    if fingerprint != schedule["fingerprint"]:
        return None
    status_value = state["status"]
    if status_value not in {"waiting", "prepared"}:
        raise ValueError("cron interval state has an invalid status")
    due = state["next_due_unix_ns"]
    if isinstance(due, bool) or not isinstance(due, int) or due < 0:
        raise ValueError("cron interval state next_due_unix_ns must be unsigned")
    if status_value == "waiting":
        if any(
            state[field] is not None
            for field in ("artifact_label", "manifest_hash")
        ):
            raise ValueError("cron waiting interval state carries artifact fields")
        return state
    label = state["artifact_label"]
    manifest_hash = state["manifest_hash"]
    if not isinstance(label, str) or not label:
        raise ValueError("cron prepared interval state has no artifact label")
    if not isinstance(manifest_hash, str) or re.fullmatch(r"[0-9a-f]{64}", manifest_hash) is None:
        raise ValueError("cron prepared interval state has an invalid manifest hash")
    return state


def commit_interval_state(
    schedule,
    status_value,
    next_due_unix_ns,
    label=None,
    manifest_hash=None,
):
    atomic_write_json(
        INTERVAL_STATE_PATH,
        {
            "schema_version": 1,
            "schedule_fingerprint": schedule["fingerprint"],
            "status": status_value,
            "next_due_unix_ns": next_due_unix_ns,
            "artifact_label": label,
            "manifest_hash": manifest_hash,
        },
    )


def prepared_manifest_path(label, manifest_hash):
    path = os.path.join(PAYLOADS_DIR, f"artifact-{manifest_hash}.artifact.json")
    body = read_regular_file_bounded(
        path,
        HOST_IO_LIMITS["max_manifest_bytes"],
        "cron prepared artifact manifest",
        private=True,
    )
    if octopus_content_hash(body) != manifest_hash:
        raise RuntimeError("cron prepared artifact manifest hash mismatch")
    try:
        manifest = json.loads(body)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise RuntimeError("cron prepared artifact manifest is malformed") from error
    if not isinstance(manifest, dict):
        raise RuntimeError("cron prepared artifact manifest must be an object")
    exact_keys(
        manifest,
        {"label", "payload_path", "payload_content_hash"},
        {"label", "payload_path", "payload_content_hash"},
        "cron prepared artifact manifest",
    )
    if manifest["label"] != label:
        raise RuntimeError("cron prepared artifact label changed")
    payload_hash = manifest["payload_content_hash"]
    if not isinstance(payload_hash, str) or re.fullmatch(r"[0-9a-f]{64}", payload_hash) is None:
        raise RuntimeError("cron prepared artifact payload hash is invalid")
    payload_path = manifest["payload_path"]
    expected_payload_path = os.path.join(
        PAYLOADS_DIR,
        f"artifact-{payload_hash}.payload",
    )
    if (
        not isinstance(payload_path, str)
        or not os.path.isabs(payload_path)
        or payload_path != expected_payload_path
    ):
        raise RuntimeError("cron prepared artifact payload path is invalid")
    payload = read_regular_file_bounded(
        payload_path,
        HOST_IO_LIMITS["max_payload_bytes"],
        "cron prepared artifact payload",
        private=True,
    )
    if octopus_content_hash(payload) != payload_hash:
        raise RuntimeError("cron prepared artifact payload hash mismatch")
    return path


def hand_artifact_with_evidence(manifest_path, evidence):
    global _NEXT_REQUEST_ID
    request_id = _NEXT_REQUEST_ID
    _NEXT_REQUEST_ID += 1
    frame = canonical_json(
        {
            "kind": "arrival",
            "request_id": request_id,
            "artifact_manifest_path": manifest_path,
            "evidence": evidence,
        }
    ) + b"\n"
    if len(frame) > HOST_IO_LIMITS["max_control_frame_bytes"]:
        raise RuntimeError("cron arrival frame exceeds max_control_frame_bytes")
    sys.stdout.buffer.write(frame)
    sys.stdout.buffer.flush()
    line = sys.stdin.buffer.readline(HOST_IO_LIMITS["max_control_frame_bytes"] + 1)
    if not line:
        raise RuntimeError("runtime verdict input closed before cron artifact admission")
    if len(line) > HOST_IO_LIMITS["max_control_frame_bytes"]:
        raise RuntimeError("cron runtime verdict exceeds max_control_frame_bytes")
    if not line.endswith(b"\n"):
        raise RuntimeError("cron runtime verdict is partial")
    try:
        message = json.loads(line)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise RuntimeError("cron runtime verdict is malformed JSON") from error
    if not isinstance(message, dict):
        raise RuntimeError("cron runtime verdict must be a JSON object")
    exact_keys(
        message,
        {"kind", "request_id", "verdict"},
        {"kind", "request_id", "verdict"},
        "cron runtime verdict",
    )
    if message.get("kind") != "delivery_verdict" or message.get("request_id") != request_id:
        raise RuntimeError(f"unexpected runtime verdict message: {message!r}")
    verdict = message.get("verdict")
    if not isinstance(verdict, dict) or len(verdict) != 1:
        raise RuntimeError(f"runtime returned malformed delivery verdict: {message!r}")
    admitted = verdict.get("admitted")
    if isinstance(admitted, dict) and set(admitted) == {"seq"}:
        sequence = admitted.get("seq")
        if isinstance(sequence, bool) or not isinstance(sequence, int) or sequence < 0:
            raise RuntimeError(f"runtime returned malformed admitted verdict: {message!r}")
        return sequence
    refused = verdict.get("refused")
    if (
        isinstance(refused, dict)
        and isinstance(refused.get("reason"), str)
        and refused["reason"]
        and set(refused) == {"reason", "detail"}
    ):
        raise DeliveryRefused(verdict)
    raise RuntimeError(f"runtime returned malformed delivery verdict: {message!r}")


def signal_ready():
    frame = canonical_json({"kind": "ready"}) + b"\n"
    if len(frame) > HOST_IO_LIMITS["max_control_frame_bytes"]:
        raise RuntimeError("cron readiness frame exceeds max_control_frame_bytes")
    sys.stdout.buffer.write(frame)
    sys.stdout.buffer.flush()


def materialize_artifact(label, payload, root=PAYLOADS_DIR):
    if not isinstance(label, str) or not label or len(label.encode("utf-8")) > 256:
        raise ValueError("cron artifact label must be a non-empty string of at most 256 bytes")
    if not isinstance(payload, bytes):
        raise TypeError("cron artifact payload must be bytes")
    if len(payload) > HOST_IO_LIMITS["max_payload_bytes"]:
        raise ValueError("cron artifact payload exceeds max_payload_bytes")
    private_directory(root)
    payload_hash = octopus_content_hash(payload)
    payload_path = os.path.join(root, f"artifact-{payload_hash}.payload")
    try:
        existing = read_regular_file_bounded(
            payload_path,
            HOST_IO_LIMITS["max_payload_bytes"],
            "cron private artifact payload",
            private=True,
        )
        if existing != payload:
            raise RuntimeError("cron private payload conflicts with its content identity")
    except FileNotFoundError:
        atomic_write_bytes(payload_path, payload)
    manifest = {
        "label": label,
        "payload_path": payload_path,
        "payload_content_hash": payload_hash,
    }
    manifest_bytes = canonical_json(manifest)
    if len(manifest_bytes) > HOST_IO_LIMITS["max_manifest_bytes"]:
        raise ValueError("cron artifact manifest exceeds max_manifest_bytes")
    manifest_hash = octopus_content_hash(manifest_bytes)
    manifest_path = os.path.join(root, f"artifact-{manifest_hash}.artifact.json")
    try:
        existing = read_regular_file_bounded(
            manifest_path,
            HOST_IO_LIMITS["max_manifest_bytes"],
            "cron private artifact manifest",
            private=True,
        )
        if existing != manifest_bytes:
            raise RuntimeError("cron private manifest conflicts with its content identity")
    except FileNotFoundError:
        atomic_write_bytes(manifest_path, manifest_bytes)
    return manifest_path, manifest_hash


def _ledger_path():
    path = os.environ.get("OCTOPUS_MOVEMENTS_JSONL")
    if not isinstance(path, str) or not path or not os.path.isabs(path):
        raise RuntimeError("cron life requires an absolute OCTOPUS_MOVEMENTS_JSONL")
    return path


def _reverse_complete_ledger_lines(path):
    flags = os.O_RDONLY | getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_NOFOLLOW", 0)
    try:
        linked = os.lstat(path)
        if stat.S_ISLNK(linked.st_mode) or not stat.S_ISREG(linked.st_mode):
            raise RuntimeError("cron ledger is not a regular file")
        descriptor = os.open(path, flags)
    except FileNotFoundError:
        return
    with os.fdopen(descriptor, "rb") as handle:
        opened = os.fstat(handle.fileno())
        linked_open = os.lstat(path)
        if (
            not stat.S_ISREG(opened.st_mode)
            or stat.S_ISLNK(linked_open.st_mode)
            or (linked.st_dev, linked.st_ino) != (opened.st_dev, opened.st_ino)
            or (linked_open.st_dev, linked_open.st_ino) != (opened.st_dev, opened.st_ino)
        ):
            raise RuntimeError("cron ledger changed while it was opened")
        snapshot_size = opened.st_size
        position = snapshot_size
        buffer = b""
        skip_tail = True
        while position > 0:
            size = min(position, 64 * 1024)
            position -= size
            handle.seek(position)
            chunk = handle.read(size)
            if len(chunk) != size:
                raise RuntimeError("cron ledger was truncated during reverse read")
            buffer = chunk + buffer
            lines = buffer.split(b"\n")
            buffer = lines[0]
            if len(buffer) > MAX_LEDGER_ROW_BYTES:
                raise RuntimeError(
                    f"cron ledger row exceeds {MAX_LEDGER_ROW_BYTES} bytes"
                )
            for line in reversed(lines[1:]):
                if len(line) > MAX_LEDGER_ROW_BYTES:
                    raise RuntimeError(
                        f"cron ledger row exceeds {MAX_LEDGER_ROW_BYTES} bytes"
                    )
                if skip_tail:
                    skip_tail = False
                    continue
                yield line
        if buffer and not skip_tail:
            yield buffer
        after = os.fstat(handle.fileno())
        linked_after = os.lstat(path)
        if (
            (after.st_dev, after.st_ino) != (opened.st_dev, opened.st_ino)
            or after.st_size < snapshot_size
            or stat.S_ISLNK(linked_after.st_mode)
            or (linked_after.st_dev, linked_after.st_ino)
            != (opened.st_dev, opened.st_ino)
        ):
            raise RuntimeError("cron ledger changed identity or shrank during reverse read")


def arrival_identity(label, manifest_hash):
    artifact_id = f"{label}:{manifest_hash}"
    return artifact_id, [
        {
            "interface_id": INTERFACE_ID,
            "artifact_id": artifact_id,
            "label": "cron.arrival.idempotency",
            "bytes": list(
                bytes.fromhex(octopus_content_hash(artifact_id.encode("utf-8")))
            ),
        }
    ]


def find_admitted_arrival_seq(artifact_id, expected_sequence=None):
    for line in _reverse_complete_ledger_lines(_ledger_path()):
        try:
            movement = json.loads(line)
        except (UnicodeDecodeError, json.JSONDecodeError) as error:
            raise RuntimeError("cron ledger contains a malformed complete row") from error
        if not isinstance(movement, dict):
            raise RuntimeError("cron ledger movement must be a JSON object")
        if (
            movement.get("kind") != "InterfaceArrival"
            or movement.get("from") != {"Interface": INTERFACE_ID}
        ):
            continue
        artifact = movement.get("artifact")
        if not isinstance(artifact, dict) or artifact.get("id") != artifact_id:
            continue
        sequence = movement.get("seq")
        if isinstance(sequence, bool) or not isinstance(sequence, int) or sequence < 0:
            raise RuntimeError("cron admitted arrival movement has an invalid seq")
        if expected_sequence is not None and sequence != expected_sequence:
            continue
        return sequence
    return None


def hand_artifact_idempotent(manifest_path, label, manifest_hash):
    artifact_id, evidence = arrival_identity(label, manifest_hash)
    admitted = find_admitted_arrival_seq(artifact_id)
    if admitted is not None:
        return admitted
    try:
        return hand_artifact_with_evidence(manifest_path, evidence)
    except DeliveryRefused as error:
        refused = error.verdict.get("refused")
        detail = refused.get("detail") if isinstance(refused, dict) else None
        if (
            isinstance(detail, dict)
            and detail.get("artifact_id") == artifact_id
            and detail.get("message")
            == "arrival evidence already exists in the movement ledger"
        ):
            admitted = find_admitted_arrival_seq(artifact_id)
            if admitted is not None:
                return admitted
        raise


def resolve_reminder(schedule):
    env_name = schedule["reminder_env"]
    value = os.environ.get(env_name, "").strip() if env_name else ""
    return value or schedule["reminder_default"]


def now(schedule):
    return datetime.now(schedule["timezone"])


def checked_time_artifact_label(label, context):
    if label != TIME_LABEL:
        raise ValueError(f"{context} has an invalid artifact label")
    return label


def prepare_time_artifact(schedule, moment, reminder, schedule_kind, scheduled_unix_ns=None, due_at=None, activity=None, window=None):
    payload_value = {
        "source": "cron",
        "source_interface": INTERFACE_ID,
        "schedule_kind": schedule_kind,
        "timezone": str(moment.tzinfo),
        "now_unix_ms": int(moment.timestamp() * 1000),
        "now_iso": moment.isoformat(),
        "reminder": reminder,
    }
    if scheduled_unix_ns is not None:
        payload_value["scheduled_unix_ns"] = scheduled_unix_ns
    if due_at is not None:
        payload_value["due_at"] = due_at
    if activity is not None:
        payload_value["last_activity"] = activity
    if window is not None:
        payload_value["activity_window"] = window
    if reminder:
        payload_value["task"] = reminder
    payload = json.dumps(
        payload_value,
        separators=(",", ":"),
        sort_keys=True,
    ).encode("utf-8")
    manifest_path, manifest_hash = materialize_artifact(TIME_LABEL, payload)
    return TIME_LABEL, manifest_path, manifest_hash


def deliver_time_artifact(schedule, label, manifest_path, manifest_hash):
    return hand_artifact_idempotent(manifest_path, label, manifest_hash)


def emit_time_artifact(moment, reminder, schedule_kind, schedule):
    label, manifest_path, manifest_hash = prepare_time_artifact(
        schedule,
        moment,
        reminder,
        schedule_kind,
    )
    return deliver_time_artifact(schedule, label, manifest_path, manifest_hash)


def next_interval_due(due_unix_ns, interval_ns, observed_unix_ns):
    next_due = due_unix_ns + interval_ns
    if next_due <= observed_unix_ns:
        skipped = (observed_unix_ns - next_due) // interval_ns + 1
        next_due += skipped * interval_ns
    return next_due


def one_shot_state_path(schedule):
    return os.path.join(VAR_DIR, "one-shot", f"{schedule['due_unix_ns']}.json")


def load_one_shot_state(schedule):
    try:
        state = load_json_object(
            one_shot_state_path(schedule),
            "cron one-shot state",
            HOST_IO_LIMITS["max_control_frame_bytes"],
            private=True,
        )
    except FileNotFoundError:
        return None
    fields = {
        "schema_version", "schedule_fingerprint", "due_unix_ns", "status",
        "artifact_label", "manifest_hash", "admitted_seq",
    }
    exact_keys(state, fields, fields, "cron one-shot state")
    if type(state["schema_version"]) is not int or state["schema_version"] != 1:
        raise ValueError("cron one-shot state schema_version is unsupported")
    if state["due_unix_ns"] != str(schedule["due_unix_ns"]):
        raise ValueError("cron one-shot state deadline conflicts with its schedule")
    if state["schedule_fingerprint"] != schedule["fingerprint"]:
        raise ValueError("cron one-shot configuration changed for an already prepared deadline")
    if state["status"] not in {"prepared", "admitted"}:
        raise ValueError("cron one-shot state has an invalid status")
    checked_time_artifact_label(state["artifact_label"], "cron one-shot state")
    if not isinstance(state["manifest_hash"], str) or re.fullmatch(r"[0-9a-f]{64}", state["manifest_hash"]) is None:
        raise ValueError("cron one-shot state has an invalid manifest hash")
    sequence = state["admitted_seq"]
    if state["status"] == "prepared" and sequence is not None:
        raise ValueError("cron prepared one-shot state already carries admitted_seq")
    if state["status"] == "admitted" and (type(sequence) is not int or sequence < 0):
        raise ValueError("cron admitted one-shot state has an invalid admitted_seq")
    return state


def run_one_shot(schedule, reminder):
    state = load_one_shot_state(schedule)
    if state is None:
        while True:
            remaining = (schedule["due_unix_ns"] - time.time_ns()) / 1_000_000_000
            if remaining <= 0:
                break
            time.sleep(remaining)
        label, manifest_path, manifest_hash = prepare_time_artifact(
            schedule, now(schedule), reminder, "one_shot", schedule["due_unix_ns"],
            due_at=schedule["due_at"],
        )
        state = {
            "schema_version": 1,
            "schedule_fingerprint": schedule["fingerprint"],
            "due_unix_ns": str(schedule["due_unix_ns"]),
            "status": "prepared",
            "artifact_label": label,
            "manifest_hash": manifest_hash,
            "admitted_seq": None,
        }
        atomic_write_json(one_shot_state_path(schedule), state)
    else:
        label = state["artifact_label"]
        manifest_hash = state["manifest_hash"]
        manifest_path = prepared_manifest_path(label, manifest_hash)
    if state["status"] == "admitted":
        artifact_id, _ = arrival_identity(label, manifest_hash)
        if find_admitted_arrival_seq(artifact_id, state["admitted_seq"]) is None:
            raise RuntimeError("cron one-shot completion is missing its recorded arrival evidence")
        return state["admitted_seq"]
    sequence = deliver_time_artifact(schedule, label, manifest_path, manifest_hash)
    state["status"] = "admitted"
    state["admitted_seq"] = sequence
    atomic_write_json(one_shot_state_path(schedule), state)
    return sequence


def emit_interval_due(schedule, reminder, state):
    due_unix_ns = state["next_due_unix_ns"]
    if state["status"] == "prepared":
        label = state["artifact_label"]
        manifest_hash = state["manifest_hash"]
        manifest_path = prepared_manifest_path(label, manifest_hash)
    else:
        label, manifest_path, manifest_hash = prepare_time_artifact(
            schedule,
            now(schedule),
            reminder,
            "interval",
            due_unix_ns,
        )
        commit_interval_state(
            schedule,
            "prepared",
            due_unix_ns,
            label,
            manifest_hash,
        )
    admitted_seq = deliver_time_artifact(schedule, label, manifest_path, manifest_hash)
    next_due = next_interval_due(
        due_unix_ns,
        schedule["interval_ns"],
        time.time_ns(),
    )
    commit_interval_state(schedule, "waiting", next_due)
    return admitted_seq


def emit_fire_on_start(moment, reminder, schedule_kind, schedule):
    state = load_fire_on_start_state(schedule)
    if state is not None and state["status"] in {
        "admitted",
        "completed_without_artifact",
    }:
        return state.get("admitted_seq")
    if state is not None:
        label = state["artifact_label"]
        manifest_hash = state["manifest_hash"]
        manifest_path = prepared_manifest_path(label, manifest_hash)
    else:
        label, manifest_path, manifest_hash = prepare_time_artifact(
            schedule,
            moment,
            reminder,
            schedule_kind,
        )
        commit_fire_on_start_state(
            schedule,
            "prepared",
            label,
            manifest_hash,
            None,
        )
    admitted_seq = deliver_time_artifact(schedule, label, manifest_path, manifest_hash)
    commit_fire_on_start_state(
        schedule,
        "admitted",
        label,
        manifest_hash,
        admitted_seq,
    )
    return admitted_seq


def run_interval(schedule, reminder):
    if schedule["fire_on_start"] and not fire_on_start_completed(schedule):
        emit_fire_on_start(now(schedule), reminder, "interval", schedule)
    state = load_interval_state(schedule)
    if state is None:
        state = {
            "status": "waiting",
            "next_due_unix_ns": time.time_ns() + schedule["interval_ns"],
        }
        commit_interval_state(
            schedule,
            state["status"],
            state["next_due_unix_ns"],
        )
    while True:
        if state["status"] == "prepared":
            emit_interval_due(schedule, reminder, state)
            state = load_interval_state(schedule)
            continue
        remaining = (state["next_due_unix_ns"] - time.time_ns()) / 1_000_000_000
        if remaining > 0:
            time.sleep(min(remaining, 60.0))
            state = load_interval_state(schedule)
            continue
        emit_interval_due(schedule, reminder, state)
        state = load_interval_state(schedule)


def run_cron(schedule, reminder):
    compiled = compile_cron(schedule["cron"])
    last_minute = load_last_fired_minute()
    if schedule["fire_on_start"] and not fire_on_start_completed(schedule):
        start_state = load_fire_on_start_state(schedule)
        if start_state is not None and start_state["status"] == "prepared":
            emit_fire_on_start(now(schedule), reminder, "cron", schedule)
        else:
            moment = now(schedule)
            minute = moment.replace(second=0, microsecond=0)
            minute_key = minute.isoformat(timespec="minutes")
            if cron_matches(compiled, minute) and minute_key != last_minute:
                emit_fire_on_start(minute, reminder, "cron", schedule)
                last_minute = minute_key
                commit_last_fired_minute(minute_key)
            else:
                commit_fire_on_start_state(
                    schedule,
                    "completed_without_artifact",
                )
    while True:
        current = now(schedule)
        scheduled = next_cron_minute(compiled, current)
        time.sleep(max(0.0, scheduled.timestamp() - current.timestamp()))
        moment = now(schedule)
        minute = moment.replace(second=0, microsecond=0)
        minute_key = minute.isoformat(timespec="minutes")
        if cron_matches(compiled, minute) and minute_key != last_minute:
            emit_time_artifact(minute, reminder, "cron", schedule)
            last_minute = minute_key
            commit_last_fired_minute(minute_key)


def validate_inactivity(value):
    exact_keys(value, {"seconds", "interface_match", "exclude_interface_match", "within_ee"}, {"seconds", "interface_match"}, "schedule.inactivity")
    seconds = value["seconds"]
    if type(seconds) not in {int, float} or not math.isfinite(seconds) or seconds < 0.001:
        raise ValueError("inactivity.seconds must be finite and at least 0.001")
    for field in ("interface_match", "exclude_interface_match"):
        if field not in value:
            continue
        selector = value[field]
        exact_keys(selector, {"interface_type", "role", "interface_driver_ref"}, set(), f"inactivity.{field}")
        if not selector or any(not isinstance(v, str) or not v for v in selector.values()):
            raise ValueError(f"inactivity.{field} requires non-empty descriptor fields")
    if "within_ee" in value and (not isinstance(value["within_ee"], str) or REGISTRY_ID.fullmatch(value["within_ee"]) is None):
        raise ValueError("inactivity.within_ee must be a registry-safe EE id")


class RunActivity:
    def __init__(self, schedule):
        self.schedule = schedule
        self.documents = {}
        self.offset = 0
        self.sequence = -1
        self.ledger_identity = None
        self.latest_by_run = {}
        self.latest_inputs_by_run = {}
        self.input_sources = {}
        self.reported = set()
        self.excluded_runs = set()
        self.excluded_sources = {}

    def document(self, path):
        info = os.stat(path, follow_symlinks=False)
        fingerprint = (info.st_dev, info.st_ino, info.st_size, info.st_mtime_ns)
        cached = self.documents.get(path)
        if cached is None or cached[0] != fingerprint:
            cached = (fingerprint, load_json_object(path, "inactivity source", MAX_RUN_RECORD_BYTES))
            self.documents[path] = cached
        return cached[1]

    def report(self, path, error):
        key = (path, str(error))
        if key not in self.reported and len(self.reported) < 64:
            self.reported.add(key)
            sys.stderr.write(f"inactivity source contributes no activity: {path}: {error}\n")
            sys.stderr.flush()

    def ee_chain(self, ee_id, seen):
        chain = []
        while ee_id is not None:
            if REGISTRY_ID.fullmatch(ee_id) is None or ee_id in chain or len(chain) > 64:
                raise ValueError(f"EE chain of '{ee_id}' is invalid")
            path = os.path.join(root_slots()["ee_instances_root"], f"{ee_id}.json")
            record = self.document(path)
            seen.add(path)
            if record.get("ee_id") != ee_id:
                raise ValueError(f"EE record '{path}' does not match its id")
            chain.append(ee_id)
            ee_id = record.get("parent_ee_id")
            if ee_id is not None and not isinstance(ee_id, str):
                raise ValueError(f"EE record '{path}' has an invalid parent")
        return chain

    def selected_runs(self):
        slots = root_slots()
        selected = set()
        autonomous_excluded = set()
        seen = set()
        selector = self.schedule["inactivity"]["interface_match"]
        exclusion = self.schedule["inactivity"].get("exclude_interface_match")
        within_ee = self.schedule["inactivity"].get("within_ee")
        for entry in os.scandir(slots["run_records_root"]):
            if not entry.name.endswith(".json"):
                continue
            seen.add(entry.path)
            try:
                run = self.document(entry.path)
                if run.get("id") != entry.name[:-5] or not isinstance(run.get("checkpoints"), list):
                    raise ValueError("invalid run identity")
                interfaces = set()
                for checkpoint in run["checkpoints"]:
                    for binding in [checkpoint.get("interface_binding"), *checkpoint.get("outbound_interface_bindings", [])]:
                        if isinstance(binding, dict) and isinstance(binding.get("interface"), str):
                            interfaces.add(binding["interface"])
                excluded = INTERFACE_ID in interfaces
                if within_ee is not None and within_ee not in self.ee_chain(run.get("ee"), seen):
                    continue
                matched = False
                for interface_id in sorted(interfaces):
                    if REGISTRY_ID.fullmatch(interface_id) is None:
                        raise ValueError(f"invalid interface id '{interface_id}'")
                    path = os.path.join(slots["registry_interface_root"], interface_id, "interface.json")
                    seen.add(path)
                    try:
                        descriptor = self.document(path)
                    except FileNotFoundError:
                        continue
                    if descriptor.get("id") != interface_id:
                        raise ValueError(f"interface '{interface_id}' does not match its folder")
                    matched = matched or all(descriptor.get(key) == value for key, value in selector.items())
                    excluded = excluded or (exclusion is not None and all(descriptor.get(key) == value for key, value in exclusion.items()))
                if matched:
                    selected.add(run["id"])
                    if excluded:
                        autonomous_excluded.add(run["id"])
            except (OSError, ValueError) as error:
                self.report(entry.path, error)
        self.documents = {path: cached for path, cached in self.documents.items() if path in seen}
        return selected, autonomous_excluded

    def latest(self):
        selected, autonomous_excluded = self.selected_runs()
        path = _ledger_path()
        try:
            descriptor = os.open(path, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0))
        except FileNotFoundError:
            if self.ledger_identity is not None:
                raise RuntimeError("inactivity ledger disappeared")
            return None
        with os.fdopen(descriptor, "rb") as handle:
            info = os.fstat(handle.fileno())
            identity = (info.st_dev, info.st_ino)
            if not stat.S_ISREG(info.st_mode) or self.ledger_identity not in {None, identity} or info.st_size < self.offset:
                raise RuntimeError("inactivity ledger changed identity or shrank")
            self.ledger_identity = identity
            handle.seek(self.offset)
            while handle.tell() < info.st_size:
                line = handle.readline(min(MAX_LEDGER_ROW_BYTES + 1, info.st_size - handle.tell()))
                if len(line) > MAX_LEDGER_ROW_BYTES:
                    raise RuntimeError("inactivity ledger row exceeds its IO limit")
                if not line.endswith(b"\n"):
                    break
                movement = json.loads(line)
                if not isinstance(movement, dict) or type(movement.get("seq")) is not int or movement["seq"] != self.sequence + 1:
                    raise RuntimeError("inactivity ledger is not contiguous")
                self.sequence = movement["seq"]
                self.offset = handle.tell()
                if movement.get("artifact") is None:
                    continue
                metadata = movement.get("invocation_metadata") or {}
                provenance = metadata.get("runtime") or metadata.get("mark_outcome") or {}
                run_id = provenance.get("run")
                if run_id is None:
                    continue
                recorded = movement.get("recorded_at_unix_ms")
                if not isinstance(run_id, str) or type(recorded) is not int or recorded < 0:
                    raise RuntimeError("inactivity movement has invalid run or time provenance")
                activity = {"run": run_id, "seq": self.sequence, "recorded_at_unix_ms": recorded}
                if movement.get("kind") == "InterfaceArrival":
                    source = movement.get("from")
                    condition = self.schedule["inactivity"]
                    if source == {"Interface": INTERFACE_ID} or self.source_matches(source, condition.get("exclude_interface_match"), self.excluded_sources):
                        self.excluded_runs.add(run_id)
                    if self.source_matches(source, condition["interface_match"], self.input_sources):
                        self.latest_inputs_by_run[run_id] = activity
                self.latest_by_run[run_id] = activity
            after = os.stat(path, follow_symlinks=False)
            if (after.st_dev, after.st_ino) != identity or after.st_size < info.st_size:
                raise RuntimeError("inactivity ledger changed during read")
        excluded = autonomous_excluded | self.excluded_runs
        candidates = ((self.latest_inputs_by_run if run_id in excluded else self.latest_by_run).get(run_id) for run_id in selected)
        return max((row for row in candidates if row is not None), key=lambda row: row["seq"], default=None)

    def source_matches(self, endpoint, selector, cache):
        interface_id = endpoint.get("Interface") if isinstance(endpoint, dict) else None
        if selector is None or not isinstance(interface_id, str) or REGISTRY_ID.fullmatch(interface_id) is None:
            return False
        if interface_id not in cache:
            path = os.path.join(root_slots()["registry_interface_root"], interface_id, "interface.json")
            try:
                descriptor = load_json_object(path, "inactivity source", MAX_RUN_RECORD_BYTES)
            except FileNotFoundError:
                return False
            except (OSError, ValueError) as error:
                self.report(path, error)
                return False
            cache[interface_id] = descriptor.get("id") == interface_id and all(
                descriptor.get(key) == value for key, value in selector.items()
            )
        return cache[interface_id]


class InactivityTrigger:
    def __init__(self, schedule, reminder):
        self.schedule = schedule
        self.reminder = reminder
        self.activity = RunActivity(schedule)
        try:
            self.state = load_json_object(INACTIVITY_STATE_PATH, "inactivity state", MAX_LEDGER_ROW_BYTES, private=True)
        except FileNotFoundError:
            self.state = {"schema_version": 1, "status": "idle", "consumed_seq": -1, "activity": None, "manifest_hash": None, "admitted_seq": None}
        state = self.state
        exact_keys(state, {"schema_version", "status", "consumed_seq", "activity", "manifest_hash", "admitted_seq", "schedule_fingerprint", "inactivity", "next_due_unix_ms", "artifact_label"}, {"schema_version", "status", "consumed_seq", "activity", "manifest_hash", "admitted_seq"}, "inactivity state")
        if state.get("schema_version") != 1 or state.get("status") not in {"idle", "waiting", "prepared", "admitted"}:
            raise ValueError("inactivity state has an invalid schema or status")
        if type(state.get("consumed_seq")) is not int or state["consumed_seq"] < -1:
            raise ValueError("inactivity state has an invalid consumed sequence")
        activity = state.get("activity")
        if activity is not None and (not isinstance(activity, dict) or type(activity.get("seq")) is not int or activity["seq"] < 0 or type(activity.get("recorded_at_unix_ms")) is not int or not isinstance(activity.get("run"), str)):
            raise ValueError("inactivity state has invalid activity evidence")
        if state["status"] in {"prepared", "admitted"}:
            if activity is None or not isinstance(state.get("manifest_hash"), str) or re.fullmatch(r"[0-9a-f]{64}", state["manifest_hash"]) is None:
                raise ValueError("inactivity state has invalid prepared evidence")
            artifact_id, _ = arrival_identity(self.artifact_label(), state["manifest_hash"])
            admitted = find_admitted_arrival_seq(artifact_id, state.get("admitted_seq"))
            if state["status"] == "admitted" and (type(state.get("admitted_seq")) is not int or admitted is None):
                raise RuntimeError("inactivity completion is missing its recorded arrival evidence")
            if admitted is not None:
                state.update(status="admitted", consumed_seq=activity["seq"], admitted_seq=admitted)
                self.save()

    def save(self):
        atomic_write_json(INACTIVITY_STATE_PATH, self.state)

    def artifact_label(self):
        return checked_time_artifact_label(self.state.get("artifact_label", TIME_LABEL), "inactivity state")

    def tick(self):
        latest = self.activity.latest()
        state = self.state
        if self.activity.sequence < state["consumed_seq"]:
            raise RuntimeError("inactivity completion refers to missing ledger history")
        if state["status"] == "prepared":
            return self.deliver()
        due = None if latest is None else latest["recorded_at_unix_ms"] + round(self.schedule["inactivity"]["seconds"] * 1000)
        if latest is None or latest["seq"] <= state["consumed_seq"]:
            if state["status"] not in {"idle", "admitted"}:
                state.update(status="idle", activity=None, next_due_unix_ms=None, manifest_hash=None, admitted_seq=None)
                state.pop("artifact_label", None)
                self.save()
            return None
        if state.get("activity") != latest or state.get("schedule_fingerprint") != self.schedule["fingerprint"]:
            state.update(status="waiting", activity=latest, schedule_fingerprint=self.schedule["fingerprint"], inactivity=self.schedule["inactivity"], next_due_unix_ms=due, manifest_hash=None, admitted_seq=None)
            state.pop("artifact_label", None)
            self.save()
        if time.time_ns() // 1_000_000 < due:
            return due
        window = {"after_seq": state["consumed_seq"] if state["consumed_seq"] >= 0 else None, "through_seq": latest["seq"]}
        label, _, manifest_hash = prepare_time_artifact(self.schedule, now(self.schedule), self.reminder, "inactivity", due * 1_000_000, activity=latest, window=window)
        state.update(status="prepared", manifest_hash=manifest_hash, artifact_label=label)
        self.save()
        return self.deliver()

    def deliver(self):
        state = self.state
        manifest_hash = state["manifest_hash"]
        label = self.artifact_label()
        manifest_path = prepared_manifest_path(label, manifest_hash)
        admitted = hand_artifact_idempotent(manifest_path, label, manifest_hash)
        state.update(status="admitted", consumed_seq=state["activity"]["seq"], admitted_seq=admitted, next_due_unix_ms=None)
        self.save()
        return None


def run_inactivity(schedule, reminder):
    trigger = InactivityTrigger(schedule, reminder)
    while True:
        due = trigger.tick()
        remaining = 5 if due is None else max(0.05, (due - time.time_ns() // 1_000_000) / 1000)
        time.sleep(min(5, remaining))


def main():
    os.umask(0o077)
    private_directory(VAR_DIR)
    private_directory(PAYLOADS_DIR)
    schedule = load_schedule()
    if schedule is None:
        sys.stderr.write(f"cron life disabled: interface={INTERFACE_ID}\n")
        sys.stderr.flush()
        signal_ready()
        while schedule is None:
            time.sleep(5)
            schedule = load_schedule()
    reminder = resolve_reminder(schedule)
    startup = (
        f"cron life up: interface={INTERFACE_ID} timezone={schedule['timezone_name']!r} "
        f"cron={schedule['cron']!r} interval_seconds={schedule['interval_seconds']!r} "
        f"due_at={schedule['due_at']!r}\n"
    )
    if len(startup.encode("utf-8")) > HOST_IO_LIMITS["max_stderr_bytes"]:
        raise RuntimeError("cron startup evidence exceeds max_stderr_bytes")
    sys.stderr.write(startup)
    sys.stderr.flush()
    signal_ready()
    if schedule["inactivity"] is not None:
        run_inactivity(schedule, reminder)
    elif schedule["due_unix_ns"] is not None:
        run_one_shot(schedule, reminder)
        while True:
            signal.pause()
    elif schedule["cron"] is not None:
        run_cron(schedule, reminder)
    else:
        run_interval(schedule, reminder)


if __name__ == "__main__":
    main()
