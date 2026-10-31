#!/usr/bin/env python3
import base64
import importlib.util
import json
import hashlib
import math
import os
import re
import signal
import socket
import sqlite3
import stat
import struct
import subprocess
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
RUN_PLANS_DIR = os.path.join(VAR_DIR, "run-plans")
ENV_NAME = re.compile(r"[A-Z_][A-Z0-9_]*\Z")
WEBSOCKET_GUID = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11"
REGISTRY_ID = re.compile(r"[a-z0-9][a-z0-9_-]*\Z")
TOOL_ID = re.compile(r"[a-z0-9][a-z0-9._-]*\Z")
TIME_LABEL = "cron.current_time"
TASK_TIME_LABEL = re.compile(r"cron\.current_time\.[0-9a-f]{24}\Z")

_NEXT_REQUEST_ID = 1
_TEMP_SEQUENCE = 1
_CARRY_FORWARD_INDEX_WARNING = None
_FRONTIER_INDEX_WARNING = None


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
MAX_WEBSOCKET_FRAME_BYTES = HOST_IO_LIMITS["max_control_frame_bytes"]
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


def authored_interface_binding(interface):
    if not isinstance(interface, str) or not interface or interface.strip() != interface:
        raise ValueError("interface binding identity must be a non-empty trimmed string")
    return {
        "interface": interface,
        "config": {"artifact_policy": {"mode": "all"}},
    }


def interface_binding_name(value, label):
    if not isinstance(value, dict):
        raise ValueError(f"{label} must be a JSON object")
    exact_keys(value, {"interface", "config"}, {"interface", "config"}, label)
    interface = value["interface"]
    if not isinstance(interface, str) or not interface or interface.strip() != interface:
        raise ValueError(f"{label}.interface must be a non-empty trimmed string")
    config = value["config"]
    if not isinstance(config, dict):
        raise ValueError(f"{label}.config must be a JSON object")
    exact_keys(config, {"artifact_policy"}, {"artifact_policy"}, f"{label}.config")
    policy = config["artifact_policy"]
    if not isinstance(policy, dict):
        raise ValueError(f"{label}.config.artifact_policy must be a JSON object")
    mode = policy.get("mode")
    if mode == "all":
        exact_keys(policy, {"mode"}, {"mode"}, f"{label}.config.artifact_policy")
    elif mode == "allow":
        exact_keys(
            policy,
            {"mode", "names", "namespaces"},
            {"mode", "names", "namespaces"},
            f"{label}.config.artifact_policy",
        )
        names = policy["names"]
        namespaces = policy["namespaces"]
        if not isinstance(names, list) or not isinstance(namespaces, list):
            raise ValueError(f"{label}.config.artifact_policy allow lists must be arrays")
        for field, values in (("names", names), ("namespaces", namespaces)):
            if any(not isinstance(item, str) or not item or item.strip() != item for item in values):
                raise ValueError(f"{label}.config.artifact_policy.{field} must contain non-empty trimmed strings")
            if len(set(values)) != len(values):
                raise ValueError(f"{label}.config.artifact_policy.{field} repeats a value")
        if not names and not namespaces:
            raise ValueError(f"{label}.config.artifact_policy allow mode grants nothing")
        if any(namespace.endswith(".") for namespace in namespaces):
            raise ValueError(f"{label}.config.artifact_policy namespace must not end with '.'")
    else:
        raise ValueError(f"{label}.config.artifact_policy.mode must be 'all' or 'allow'")
    return interface


def scheduled_topology(record, run, authoring):
    exact_keys(
        record,
        {
            "id",
            "ee",
            "worker",
            "trace",
            "status",
            "interface_state",
            "checkpoints",
            "active_checkpoints",
        },
        {
            "id",
            "ee",
            "worker",
            "trace",
            "status",
            "interface_state",
            "checkpoints",
            "active_checkpoints",
        },
        "run_template",
    )
    if record["id"] != run:
        raise ValueError(
            f"run_template id {record['id']!r} does not match declared run identity {run!r}"
        )
    for field in ("ee", "worker"):
        value = record[field]
        if not isinstance(value, str) or not value.strip():
            raise ValueError(f"run_template.{field} must be non-empty")
    if not isinstance(record["trace"], str) or not record["trace"].strip():
        raise ValueError("run_template.trace must be non-empty")
    checkpoints = record["checkpoints"]
    active = record["active_checkpoints"]
    if not isinstance(checkpoints, list) or not isinstance(active, list):
        raise ValueError("run_template checkpoints and active_checkpoints must be arrays")
    if any(not isinstance(checkpoint, dict) for checkpoint in checkpoints):
        raise ValueError("run_template checkpoints contains a non-object")
    if any(not isinstance(checkpoint, str) or not checkpoint for checkpoint in active):
        raise ValueError("run_template active_checkpoints contains an invalid id")
    if len(set(active)) != len(active):
        raise ValueError("run_template active_checkpoints repeats an id")
    states = record.get("interface_state", {})
    if not isinstance(states, dict):
        raise ValueError("run_template.interface_state must be an object")
    state = states.get(INTERFACE_ID)
    if not isinstance(state, dict):
        raise ValueError(f"run_template has no interface_state for {INTERFACE_ID!r}")
    generation = state.get("authoring_generation")
    frontier = state.get("frontier_checkpoint_ids")
    if isinstance(generation, bool) or not isinstance(generation, int) or generation < 0:
        raise ValueError("run_template authoring_generation must be unsigned")
    if (
        not isinstance(frontier, list)
        or len(frontier) != 3
        or any(not isinstance(value, str) or not value for value in frontier)
        or len(set(frontier)) != 3
    ):
        raise ValueError("run_template frontier_checkpoint_ids must contain three unique ids")
    by_id = {}
    for index, checkpoint in enumerate(checkpoints):
        exact_keys(
            checkpoint,
            {"id", "run", "trace", "epoch", "interface_binding", "outbound_interface_bindings", "artifact_bindings", "required_verified_artifacts"},
            {"id", "run", "trace", "epoch", "interface_binding", "outbound_interface_bindings", "artifact_bindings", "required_verified_artifacts"},
            f"run_template checkpoints[{index}]",
        )
        checkpoint_id = checkpoint.get("id")
        if not isinstance(checkpoint_id, str) or not checkpoint_id:
            raise ValueError("run_template checkpoint has no non-empty id")
        if checkpoint_id in by_id:
            raise ValueError(f"run_template repeats checkpoint {checkpoint_id!r}")
        if checkpoint.get("run") != run:
            raise ValueError(f"run_template checkpoint {checkpoint_id!r} belongs to another run")
        if not isinstance(checkpoint["trace"], str) or not checkpoint["trace"].strip():
            raise ValueError(f"run_template checkpoint {checkpoint_id!r} trace must be non-empty")
        epoch = checkpoint["epoch"]
        if isinstance(epoch, bool) or not isinstance(epoch, int) or epoch < 0:
            raise ValueError(f"run_template checkpoint {checkpoint_id!r} epoch must be unsigned")
        interface_binding_name(
            checkpoint["interface_binding"],
            f"run_template checkpoint {checkpoint_id}.interface_binding",
        )
        by_id[checkpoint_id] = checkpoint
    if any(frontier_id not in by_id or active.count(frontier_id) != 1 for frontier_id in frontier):
        raise ValueError("run_template frontier is not exactly active")
    trigger, call, result = (by_id[frontier_id] for frontier_id in frontier)
    expected_inbound = (
        INTERFACE_ID,
        authoring["source_interface"],
        authoring["environment_interface"],
    )
    for checkpoint, inbound in zip((trigger, call, result), expected_inbound):
        checkpoint_id = checkpoint.get("id")
        if interface_binding_name(
            checkpoint.get("interface_binding"),
            f"run_template checkpoint {checkpoint_id}.interface_binding",
        ) != inbound:
            raise ValueError(
                f"run_template checkpoint {checkpoint_id!r} must bind {inbound!r}"
            )
        for field in (
            "outbound_interface_bindings",
            "artifact_bindings",
            "required_verified_artifacts",
        ):
            value = checkpoint.get(field, [])
            if not isinstance(value, list):
                raise ValueError(
                    f"run_template checkpoint {checkpoint_id!r} {field} must be an array"
                )
        for outbound_index, outbound_binding in enumerate(checkpoint["outbound_interface_bindings"]):
            interface_binding_name(
                outbound_binding,
                f"run_template checkpoint {checkpoint_id}.outbound_interface_bindings[{outbound_index}]",
            )
    if [
        interface_binding_name(binding, f"run_template authoring call outbound_interface_bindings[{index}]")
        for index, binding in enumerate(call.get("outbound_interface_bindings", []))
    ] != [authoring["environment_interface"]]:
        raise ValueError("run_template authoring call must have exactly its environment outbound")
    return {
        "ee": record["ee"],
        "worker": record["worker"],
        "checkpoint": trigger["id"],
        "outbound_bindings": [
            interface_binding_name(binding, f"run_template trigger outbound_interface_bindings[{index}]")
            for index, binding in enumerate(trigger.get("outbound_interface_bindings", []))
        ],
        "artifact_bindings": trigger.get("artifact_bindings", []),
        "required_verified_artifacts": trigger.get("required_verified_artifacts", []),
    }


def scheduled_target(value, topology, carry_forward):
    if not isinstance(value, dict):
        raise ValueError("cron activation config.target must be a JSON object")
    mode = value.get("mode")
    if mode == "existing_run":
        exact_keys(value, {"mode"}, {"mode"}, "cron target")
        return {"mode": mode}
    if mode != "new_run":
        raise ValueError("cron target.mode must be 'existing_run' or 'new_run'")
    exact_keys(
        value,
        {
            "mode",
            "ee_id",
            "worker_interface_id",
            "provider_interface_id",
            "reasoning_effort",
            "client_surface_role",
            "id_prefix",
            "interface_private_state_root",
            "door_host_slots",
            "model",
            "selected_worker_environment_kinds",
            "context_artifact_bindings",
        },
        {
            "mode",
            "ee_id",
            "worker_interface_id",
            "provider_interface_id",
            "reasoning_effort",
            "client_surface_role",
            "id_prefix",
            "door_host_slots",
        },
        "cron target",
    )
    if topology["outbound_bindings"]:
        raise ValueError("new_run target controller trigger must have no outbound interfaces")
    if carry_forward is not None:
        raise ValueError("new_run target cannot carry artifacts from the controller run")
    for field in (
        "ee_id",
        "worker_interface_id",
        "provider_interface_id",
        "id_prefix",
    ):
        item = value[field]
        if not isinstance(item, str) or REGISTRY_ID.fullmatch(item) is None:
            raise ValueError(f"cron target.{field} must be a registry-safe identifier")
    if len(value["id_prefix"]) > 48:
        raise ValueError("cron target.id_prefix must not exceed 48 characters")
    if "model" in value and (not isinstance(value["model"], str) or not value["model"].strip()):
        raise ValueError("cron target.model must be non-empty")
    kinds = value.get("selected_worker_environment_kinds", [])
    if not isinstance(kinds, list) or any(not isinstance(kind, str) or REGISTRY_ID.fullmatch(kind) is None for kind in kinds) or len(set(kinds)) != len(kinds):
        raise ValueError("cron target.selected_worker_environment_kinds must contain unique kind ids")
    artifacts = value.get("context_artifact_bindings", [])
    if not isinstance(artifacts, list):
        raise ValueError("cron target.context_artifact_bindings must be an array")
    for artifact in artifacts:
        exact_keys(artifact, {"id"}, {"id"}, "cron target context artifact")
        if not isinstance(artifact["id"], str) or REGISTRY_ID.fullmatch(artifact["id"]) is None:
            raise ValueError("cron target context artifact must have a registry id")
    reasoning_effort = value["reasoning_effort"]
    if not isinstance(reasoning_effort, str) or not reasoning_effort.strip():
        raise ValueError("cron target.reasoning_effort must be non-empty")
    if value["client_surface_role"] not in {"home", "task"}:
        raise ValueError("cron target.client_surface_role must be 'home' or 'task'")
    private_root = os.path.dirname(os.path.realpath(VAR_DIR))
    if "interface_private_state_root" in value and (
        not isinstance(value["interface_private_state_root"], str)
        or os.path.realpath(value["interface_private_state_root"]) != private_root
    ):
        raise ValueError("cron target.interface_private_state_root must name this Home's interface state root")
    slots = value["door_host_slots"]
    if not isinstance(slots, dict):
        raise ValueError("cron target.door_host_slots must be a JSON object")
    normalized_slots = {}
    for interface, roots in slots.items():
        if not isinstance(interface, str) or REGISTRY_ID.fullmatch(interface) is None:
            raise ValueError("cron target.door_host_slots has an invalid interface id")
        if not isinstance(roots, dict) or not roots:
            raise ValueError(
                f"cron target.door_host_slots.{interface} must be a non-empty JSON object"
            )
        normalized_roots = {}
        for slot, root in roots.items():
            if not isinstance(slot, str) or REGISTRY_ID.fullmatch(slot) is None:
                raise ValueError(
                    f"cron target.door_host_slots.{interface} has an invalid root slot"
                )
            if not isinstance(root, str) or not os.path.isabs(root):
                raise ValueError(
                    f"cron target.door_host_slots.{interface}.{slot} must be absolute"
                )
            normalized_roots[slot] = root
        normalized_slots[interface] = normalized_roots
    return {
        "mode": mode,
        "ee_id": value["ee_id"],
        "worker_interface_id": value["worker_interface_id"],
        "provider_interface_id": value["provider_interface_id"],
        "reasoning_effort": reasoning_effort,
        "client_surface_role": value["client_surface_role"],
        "id_prefix": value["id_prefix"],
        "interface_private_state_root": private_root,
        "door_host_slots": normalized_slots,
        **{key: value[key] for key in ("model", "selected_worker_environment_kinds", "context_artifact_bindings") if key in value},
    }


def root_slots():
    try:
        value = json.loads(os.environ["OCTOPUS_ROOT_SLOTS_JSON"])
    except (KeyError, json.JSONDecodeError) as error:
        raise RuntimeError("OCTOPUS_ROOT_SLOTS_JSON is absent or malformed") from error
    if not isinstance(value, dict):
        raise RuntimeError("OCTOPUS_ROOT_SLOTS_JSON must be a JSON object")
    for name in (
        "run_records_root",
        "app_server_control_root",
        "interface_hand_in_root",
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
    exact_keys(
        config,
        {"run_template", "schedule", "authoring", "carry_forward", "target"},
        {"run_template", "schedule", "authoring", "target"},
        "cron activation config",
    )
    run_template = config["run_template"]
    if not isinstance(run_template, dict):
        raise ValueError("cron activation config.run_template must be a JSON object")
    run = run_template.get("id")
    if not isinstance(run, str) or REGISTRY_ID.fullmatch(run) is None:
        raise ValueError("cron activation config.run_template.id must be a registry-safe identifier")
    carry_forward = config.get("carry_forward")
    if carry_forward is not None:
        if not isinstance(carry_forward, dict):
            raise ValueError("cron activation config.carry_forward must be a JSON object")
        exact_keys(
            carry_forward,
            {"mark", "artifact_label"},
            {"mark", "artifact_label"},
            "cron activation config.carry_forward",
        )
        for field in ("mark", "artifact_label"):
            value = carry_forward[field]
            if not isinstance(value, str) or not value.strip():
                raise ValueError(
                    f"cron activation config.carry_forward.{field} must be non-empty"
                )
    authoring = config["authoring"]
    if not isinstance(authoring, dict):
        raise ValueError("cron activation config.authoring must be a JSON object")
    exact_keys(
        authoring,
        {
            "source_interface",
            "environment_interface",
            "file_write_tool",
            "file_write_label",
            "result_timeout_ms",
            "max_files_per_call",
        },
        {
            "source_interface",
            "environment_interface",
            "file_write_tool",
            "file_write_label",
            "result_timeout_ms",
        },
        "cron activation config.authoring",
    )
    for field in (
        "source_interface",
        "environment_interface",
        "file_write_tool",
        "file_write_label",
    ):
        value = authoring[field]
        if not isinstance(value, str) or not value.strip():
            raise ValueError(f"cron activation config.authoring.{field} must be non-empty")
    result_timeout_ms = authoring["result_timeout_ms"]
    max_files = authoring.get("max_files_per_call", 8)
    if type(max_files) is not int or max_files <= 0:
        raise ValueError("authoring.max_files_per_call must be a positive integer")
    if (
        isinstance(result_timeout_ms, bool)
        or not isinstance(result_timeout_ms, int)
        or result_timeout_ms <= 0
    ):
        raise ValueError(
            "cron activation config.authoring.result_timeout_ms must be a positive integer"
        )
    topology = scheduled_topology(run_template, run, authoring)
    target = scheduled_target(config["target"], topology, carry_forward)
    task_planner_executable = None
    if target["mode"] == "new_run":
        driver_config = descriptor.get("interface_driver_config")
        private_config = (
            driver_config.get("private_config")
            if isinstance(driver_config, dict)
            else None
        )
        task_planner_executable = (
            private_config.get("task_planner_executable")
            if isinstance(private_config, dict)
            else None
        )
        if (
            not isinstance(task_planner_executable, str)
            or not task_planner_executable
            or os.path.isabs(task_planner_executable)
            or any(
                segment in {"", ".", ".."}
                for segment in task_planner_executable.split("/")
            )
        ):
            raise ValueError(
                "new_run target requires an installation-relative task_planner_executable"
            )
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
        "run": run,
        "run_template": run_template,
        "topology": topology,
        "authoring": authoring,
        "carry_forward": carry_forward,
        "target": target,
        "task_planner_executable": task_planner_executable,
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
                    "carry_forward": carry_forward,
                    "target": target,
                    "task_planner_executable": task_planner_executable,
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
        "clock_wildcard": minute.startswith("*") or hour.startswith("*"),
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


def minute_before(minute):
    return (minute.astimezone(timezone.utc) - timedelta(minutes=1)).astimezone(minute.tzinfo)


def cron_due(compiled, minute, previous):
    if cron_matches(compiled, minute):
        return minute.fold == 0 or compiled["clock_wildcard"]
    if compiled["clock_wildcard"] or minute.utcoffset() == previous.utcoffset():
        return False
    skipped = previous.replace(tzinfo=None) + timedelta(minutes=1)
    while skipped < minute.replace(tzinfo=None):
        if cron_matches(compiled, skipped):
            return True
        skipped += timedelta(minutes=1)
    return False


def next_cron_minute(compiled, moment):
    candidate = moment.astimezone(timezone.utc).replace(second=0, microsecond=0)
    limit = candidate + timedelta(days=366 * 8)
    previous = candidate.astimezone(moment.tzinfo)
    while candidate < limit:
        candidate += timedelta(minutes=1)
        local = candidate.astimezone(moment.tzinfo)
        if cron_due(compiled, local, previous):
            return local
        previous = local
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


def scheduled_registry_roots(schedule):
    slots = root_slots()
    required = (
        "registry_interface_root",
        "registry_ee_template_root",
        "registry_tool_root",
        "registry_worker_environment_kind_root",
        "ee_instances_root",
        "run_records_root",
        "interface_life_install_root",
    )
    for name in required:
        path = slots.get(name)
        if not isinstance(path, str) or not path or not os.path.isabs(path):
            raise RuntimeError(f"new_run target requires absolute root slot {name!r}")
    registry_root = os.path.dirname(slots["registry_interface_root"])
    expected = {
        "registry_interface_root": "interface",
        "registry_ee_template_root": "ee_template",
        "registry_tool_root": "tool",
        "registry_worker_environment_kind_root": "worker_environment_kind",
    }
    for name, child in expected.items():
        if os.path.normpath(slots[name]) != os.path.normpath(
            os.path.join(registry_root, child)
        ):
            raise RuntimeError(f"new_run target registry root slot {name!r} is incoherent")
    installation_root = os.path.normpath(slots["interface_life_install_root"])
    task_planner_executable = os.path.normpath(
        os.path.join(installation_root, schedule["task_planner_executable"])
    )
    if os.path.commonpath([installation_root, task_planner_executable]) != installation_root:
        raise RuntimeError("task planner executable escapes interface_life_install_root")
    return {
        "registry_root": registry_root,
        "ee_instances_root": slots["ee_instances_root"],
        "run_records_root": slots["run_records_root"],
        "task_planner_executable": task_planner_executable,
    }


def task_plan_path(manifest_hash):
    return os.path.join(RUN_PLANS_DIR, f"{manifest_hash}.json")


def validate_task_plan(schedule, manifest_hash, state):
    exact_keys(
        state,
        {
            "schema_version",
            "schedule_fingerprint",
            "manifest_hash",
            "plan_hash",
            "controller_frontier_hashes",
            "authoring_attempt",
            "plan",
        },
        {
            "schema_version",
            "schedule_fingerprint",
            "manifest_hash",
            "plan_hash",
            "controller_frontier_hashes",
            "authoring_attempt",
            "plan",
        },
        "cron task plan",
    )
    if state["schema_version"] != 1:
        raise ValueError("cron task plan schema_version is unsupported")
    if state["schedule_fingerprint"] != schedule["fingerprint"]:
        raise ValueError("cron task plan belongs to another schedule configuration")
    if state["manifest_hash"] != manifest_hash:
        raise ValueError("cron task plan belongs to another time artifact")
    plan = state["plan"]
    if not isinstance(plan, dict):
        raise ValueError("cron task plan.plan must be a JSON object")
    observed_plan_hash = octopus_content_hash(canonical_json(plan))
    if state["plan_hash"] != observed_plan_hash:
        raise ValueError("cron task plan hash does not match its body")
    hashes = state["controller_frontier_hashes"]
    if (
        not isinstance(hashes, list)
        or any(
            not isinstance(value, str)
            or re.fullmatch(r"[0-9a-f]{64}", value) is None
            for value in hashes
        )
        or len(set(hashes)) != len(hashes)
        or len(hashes) > 1
    ):
        raise ValueError("cron task plan controller_frontier_hashes are invalid")
    validate_task_authoring_attempt(schedule, state["authoring_attempt"])
    validate_task_plan_output(schedule, manifest_hash, plan)
    return state


def validate_task_authoring_attempt(schedule, attempt):
    if attempt is None:
        return
    exact_keys(
        attempt,
        {
            "before_hash",
            "after_hash",
            "generation",
            "manifest_path",
            "artifact_id",
        },
        {
            "before_hash",
            "after_hash",
            "generation",
            "manifest_path",
            "artifact_id",
        },
        "cron task authoring attempt",
    )
    for field in ("before_hash", "after_hash"):
        if (
            not isinstance(attempt[field], str)
            or re.fullmatch(r"[0-9a-f]{64}", attempt[field]) is None
        ):
            raise ValueError(f"cron task authoring attempt {field} is invalid")
    if (
        isinstance(attempt["generation"], bool)
        or not isinstance(attempt["generation"], int)
        or attempt["generation"] <= 0
    ):
        raise ValueError("cron task authoring attempt generation is invalid")
    manifest_path = attempt["manifest_path"]
    authoring_root = os.path.join(
        root_slots()["interface_hand_in_root"],
        schedule["authoring"]["source_interface"],
    )
    if (
        not isinstance(manifest_path, str)
        or not os.path.isabs(manifest_path)
        or os.path.commonpath((authoring_root, manifest_path)) != authoring_root
    ):
        raise ValueError("cron task authoring attempt manifest_path is outside its private root")
    manifest_bytes = read_regular_file_bounded(
        manifest_path,
        MAX_AUTHORING_MANIFEST_BYTES,
        "cron task authoring manifest",
        private=True,
    )
    try:
        manifest = json.loads(manifest_bytes)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise ValueError("cron task authoring attempt manifest is malformed") from error
    if not isinstance(manifest, dict):
        raise ValueError("cron task authoring attempt manifest must be a JSON object")
    payload_hash = manifest.get("payload_content_hash")
    expected_artifact_id = f"{schedule['authoring']['file_write_label']}:{octopus_content_hash(manifest_bytes)}"
    if (
        not isinstance(payload_hash, str)
        or re.fullmatch(r"[0-9a-f]{64}", payload_hash) is None
        or attempt["artifact_id"] != expected_artifact_id
    ):
        raise ValueError("cron task authoring attempt artifact identity is invalid")


def task_plan_request(schedule, manifest_hash, label):
    target = schedule["target"]
    roots = scheduled_registry_roots(schedule)
    suffix = manifest_hash[:24]
    chat_id = f"{target['id_prefix']}_{suffix}"
    checkpoint_id = f"cp_scheduled_{suffix}"
    return {
        "registry_root": roots["registry_root"],
        "ee_instances_root": roots["ee_instances_root"],
        "run_records_root": roots["run_records_root"],
        "write_hash_algorithm": "blake3",
        "interface_private_state_root": target["interface_private_state_root"],
        "chat_id": chat_id,
        "provider_interface_id": target["provider_interface_id"],
        "reasoning_effort": target["reasoning_effort"],
        **{key: target[key] for key in ("model", "selected_worker_environment_kinds", "context_artifact_bindings") if key in target},
        "ee_id": target["ee_id"],
        "worker_interface_id": target["worker_interface_id"],
        "client_surface_role": target["client_surface_role"],
        "cycle": f"scheduled_{suffix}",
        "door_host_slots": target["door_host_slots"],
        "external_inputs": [
            {
                "checkpoint_id": checkpoint_id,
                "interface_id": INTERFACE_ID,
                "artifact_names": [label],
            }
        ],
        "existing_run_record": None,
        "conversation": {
            "ledger_offset": 0,
            "artifact_bindings": [],
            "pinned_artifact_bindings": [],
            "pinned_context_items": [],
            "continued_from": None,
        },
    }


def invoke_task_planner(schedule, manifest_hash, label):
    roots = scheduled_registry_roots(schedule)
    executable = roots["task_planner_executable"]
    request = task_plan_request(schedule, manifest_hash, label)
    try:
        completed = subprocess.run(
            [executable],
            input=canonical_json(request),
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=schedule["authoring"]["result_timeout_ms"] / 1000.0,
            check=False,
        )
    except (OSError, subprocess.SubprocessError) as error:
        raise RuntimeError(f"scheduled task planner could not execute: {error}") from error
    if completed.returncode != 0:
        detail = completed.stderr.decode("utf-8", "replace").strip()
        raise RuntimeError(
            f"scheduled task planner failed with code {completed.returncode}: {detail}"
        )
    if len(completed.stdout) > HOST_IO_LIMITS["max_payload_bytes"]:
        raise RuntimeError("scheduled task plan exceeds max_payload_bytes")
    try:
        plan = json.loads(completed.stdout)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise RuntimeError("scheduled task planner returned malformed JSON") from error
    validate_task_plan_output(schedule, manifest_hash, plan)
    return plan


def validate_task_plan_output(schedule, manifest_hash, plan):
    if not isinstance(plan, dict):
        raise ValueError("scheduled task planner output must be a JSON object")
    exact_keys(
        plan,
        {
            "run_id",
            "chat_interface_id",
            "model_interface_id",
            "worker_interface_id",
            "client_surface_role",
            "authoring_source_interface_id",
            "authoring_environment_interface_id",
            "reasoning_effort",
            "expected_before_hash_algorithm",
            "ee_template_chain",
            "capability_doors",
            "capability_tools",
            "authoring_tool_id",
            "worker_relays",
            "interface_writes",
            "run_record_content",
            "model", "service_tier", "authoring_profile", "required_interface_ids",
            "required_tool_ids", "run_writes", "removals", "publication_evidence", "publication_expansion",
        },
        {
            "run_id",
            "chat_interface_id",
            "model_interface_id",
            "worker_interface_id",
            "client_surface_role",
            "authoring_source_interface_id",
            "authoring_environment_interface_id",
            "reasoning_effort",
            "expected_before_hash_algorithm",
            "ee_template_chain",
            "capability_doors",
            "capability_tools",
            "authoring_tool_id",
            "worker_relays",
            "interface_writes",
            "run_record_content",
            "model", "service_tier", "authoring_profile", "required_interface_ids",
            "required_tool_ids", "run_writes", "removals",
        },
        "scheduled task planner output",
    )
    if plan["expected_before_hash_algorithm"] != "blake3":
        raise ValueError("scheduled task planner did not form BLAKE3 write guards")
    suffix = manifest_hash[:24]
    expected_chat = f"{schedule['target']['id_prefix']}_{suffix}"
    if plan["run_id"] != f"run_operator_chat_{expected_chat}":
        raise ValueError("scheduled task planner returned a mismatched run id")
    if plan["chat_interface_id"] != f"world_operator_chat_{expected_chat}":
        raise ValueError("scheduled task planner returned a mismatched client interface")
    if plan["client_surface_role"] != schedule["target"]["client_surface_role"]:
        raise ValueError("scheduled task planner changed the client surface role")
    if plan["reasoning_effort"] != schedule["target"]["reasoning_effort"]:
        raise ValueError("scheduled task planner changed the reasoning effort")
    if "model" in schedule["target"] and plan["model"] != schedule["target"]["model"]:
        raise ValueError("scheduled task planner changed the selected model")
    if plan["removals"] != []:
        raise ValueError("new scheduled task planning must not remove existing files")
    for field in (
        "model_interface_id",
        "worker_interface_id",
        "authoring_source_interface_id",
        "authoring_environment_interface_id",
    ):
        if not isinstance(plan[field], str) or REGISTRY_ID.fullmatch(plan[field]) is None:
            raise ValueError(f"scheduled task planner output.{field} is invalid")
    if (
        not isinstance(plan["authoring_tool_id"], str)
        or TOOL_ID.fullmatch(plan["authoring_tool_id"]) is None
    ):
        raise ValueError("scheduled task planner output.authoring_tool_id is invalid")
    for field in ("ee_template_chain", "worker_relays"):
        values = plan[field]
        if (
            not isinstance(values, list)
            or any(not isinstance(value, str) or not value for value in values)
            or len(set(values)) != len(values)
        ):
            raise ValueError(f"scheduled task planner output.{field} is invalid")
    for field in ("capability_doors", "capability_tools"):
        values = plan[field]
        if not isinstance(values, dict) or any(
            not isinstance(key, str)
            or not isinstance(value, str)
            or not key
            or not value
            for key, value in values.items()
        ):
            raise ValueError(f"scheduled task planner output.{field} is invalid")
    if not isinstance(plan["interface_writes"], list) or not plan["interface_writes"] or not isinstance(plan["run_writes"], list):
        raise ValueError("scheduled task planner returned no interface writes")
    writes = plan["interface_writes"] + plan["run_writes"]
    seen = set()
    for index, write in enumerate(writes):
        if not isinstance(write, dict):
            raise ValueError(f"scheduled task planner write #{index} is not an object")
        exact_keys(
            write,
            {"path", "content", "tag", "expected_before_hash"},
            {"path", "content", "tag", "expected_before_hash"},
            f"scheduled task planner write #{index}",
        )
        path = write["path"]
        if not isinstance(path, str) or not os.path.isabs(path) or path in seen:
            raise ValueError(f"scheduled task planner write #{index} has an invalid path")
        seen.add(path)
        if not isinstance(write["content"], str) or not isinstance(write["tag"], str):
            raise ValueError(f"scheduled task planner write #{index} has invalid content or tag")
        expected = write["expected_before_hash"]
        if expected is not None and (
            not isinstance(expected, str)
            or re.fullmatch(r"[0-9a-f]{64}", expected) is None
        ):
            raise ValueError(
                f"scheduled task planner write #{index} has an invalid expected_before_hash"
            )
    try:
        run = json.loads(plan["run_record_content"])
    except (TypeError, json.JSONDecodeError) as error:
        raise ValueError("scheduled task planner run record is malformed") from error
    if (
        not isinstance(run, dict)
        or run.get("id") != plan["run_id"]
        or run.get("ee") != schedule["target"]["ee_id"]
        or run.get("worker") != plan["worker_interface_id"]
    ):
        raise ValueError("scheduled task planner run record identity is inconsistent")
    checkpoint_id = f"cp_scheduled_{suffix}"
    matching = [
        checkpoint
        for checkpoint in run.get("checkpoints", [])
        if isinstance(checkpoint, dict) and checkpoint.get("id") == checkpoint_id
    ]
    if len(matching) != 1:
        raise ValueError("scheduled task planner omitted the exact time checkpoint")
    checkpoint = matching[0]
    if (
        interface_binding_name(
            checkpoint.get("interface_binding"),
            "scheduled task time checkpoint.interface_binding",
        ) != INTERFACE_ID
        or [
            interface_binding_name(binding, f"scheduled task time checkpoint.outbound_interface_bindings[{index}]")
            for index, binding in enumerate(checkpoint.get("outbound_interface_bindings", []))
        ] != [plan["model_interface_id"]]
        or checkpoint_id not in run.get("active_checkpoints", [])
    ):
        raise ValueError("scheduled task time checkpoint has a mismatched route")


def load_or_prepare_task_plan(schedule, manifest_hash, label):
    private_directory(RUN_PLANS_DIR)
    path = task_plan_path(manifest_hash)
    try:
        state = load_json_object(
            path,
            "cron task plan",
            HOST_IO_LIMITS["max_payload_bytes"],
            private=True,
        )
        return validate_task_plan(schedule, manifest_hash, state)
    except FileNotFoundError:
        pass
    plan = invoke_task_planner(schedule, manifest_hash, label)
    state = {
        "schema_version": 1,
        "schedule_fingerprint": schedule["fingerprint"],
        "manifest_hash": manifest_hash,
        "plan_hash": octopus_content_hash(canonical_json(plan)),
        "controller_frontier_hashes": [],
        "authoring_attempt": None,
        "plan": plan,
    }
    atomic_write_json(path, state, HOST_IO_LIMITS["max_payload_bytes"])
    return validate_task_plan(schedule, manifest_hash, state)


def commit_task_plan_state(schedule, state):
    validate_task_plan(schedule, state["manifest_hash"], state)
    atomic_write_json(task_plan_path(state["manifest_hash"]), state, HOST_IO_LIMITS["max_payload_bytes"])


def task_plan_writes(schedule, plan):
    roots = scheduled_registry_roots(schedule)
    writes = [
        {
            "path": write["path"],
            "content": write["content"],
            "expected_before_hash": write["expected_before_hash"],
        }
        for write in plan["interface_writes"] + plan["run_writes"]
    ]
    run_path = os.path.join(roots["run_records_root"], f"{plan['run_id']}.json")
    if any(write["path"] == run_path for write in writes):
        raise RuntimeError("scheduled task planner tried to write its run twice")
    writes.append(
        {
            "path": run_path,
            "content": plan["run_record_content"],
            "expected_before_hash": None,
        }
    )
    return writes


def task_writes_complete(writes):
    for write in writes:
        try:
            body = read_regular_file_bounded(
                write["path"],
                MAX_RUN_RECORD_BYTES,
                "scheduled task authored file",
            )
        except FileNotFoundError:
            return False
        if body != write["content"].encode("utf-8"):
            return False
    return True


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


def read_http_upgrade(sock, key):
    response = bytearray()
    while b"\r\n\r\n" not in response:
        chunk = sock.recv(4096)
        if not chunk:
            raise RuntimeError("authoring websocket closed during HTTP upgrade")
        response.extend(chunk)
        if len(response) > 64 * 1024:
            raise RuntimeError("authoring websocket HTTP upgrade exceeds 65536 bytes")
    header, trailing = bytes(response).split(b"\r\n\r\n", 1)
    lines = header.decode("ascii", "strict").split("\r\n")
    if not lines or not lines[0].startswith("HTTP/1.1 101 "):
        raise RuntimeError(f"authoring websocket upgrade was refused: {lines[0] if lines else ''!r}")
    headers = {}
    for line in lines[1:]:
        name, separator, value = line.partition(":")
        if not separator:
            raise RuntimeError(f"authoring websocket returned malformed HTTP header: {line!r}")
        lowered = name.strip().lower()
        if not lowered or lowered in headers:
            raise RuntimeError(f"authoring websocket repeated HTTP header: {name!r}")
        headers[lowered] = value.strip()
    expected = base64.b64encode(
        hashlib.sha1((key + WEBSOCKET_GUID).encode("ascii")).digest()
    ).decode("ascii")
    if headers.get("sec-websocket-accept") != expected:
        raise RuntimeError("authoring websocket returned an invalid Sec-WebSocket-Accept")
    return bytearray(trailing)


def websocket_frame(opcode, payload):
    if not isinstance(payload, bytes):
        raise TypeError("websocket payload must be bytes")
    size = len(payload)
    if size > MAX_WEBSOCKET_FRAME_BYTES:
        raise RuntimeError(
            f"authoring websocket frame exceeds {MAX_WEBSOCKET_FRAME_BYTES} bytes"
        )
    mask = os.urandom(4)
    if size < 126:
        header = bytes((0x80 | opcode, 0x80 | size))
    elif size <= 0xFFFF:
        header = bytes((0x80 | opcode, 0x80 | 126)) + struct.pack("!H", size)
    else:
        header = bytes((0x80 | opcode, 0x80 | 127)) + struct.pack("!Q", size)
    masked = bytes(value ^ mask[index % 4] for index, value in enumerate(payload))
    return header + mask + masked


def websocket_read_frame(sock, buffered):
    def take(size):
        while len(buffered) < size:
            chunk = sock.recv(max(4096, size - len(buffered)))
            if not chunk:
                raise RuntimeError("authoring websocket closed before its verdict frame")
            buffered.extend(chunk)
        value = bytes(buffered[:size])
        del buffered[:size]
        return value

    first, second = take(2)
    if first & 0x70:
        raise RuntimeError("authoring websocket verdict uses reserved frame bits")
    if not first & 0x80:
        raise RuntimeError("authoring websocket verdict is fragmented")
    opcode = first & 0x0F
    masked = bool(second & 0x80)
    if masked:
        raise RuntimeError("authoring websocket server returned a masked frame")
    size = second & 0x7F
    if size == 126:
        size = struct.unpack("!H", take(2))[0]
    elif size == 127:
        encoded = take(8)
        if encoded[0] & 0x80:
            raise RuntimeError("authoring websocket returned an invalid 64-bit frame length")
        size = struct.unpack("!Q", encoded)[0]
    if size > MAX_WEBSOCKET_FRAME_BYTES:
        raise RuntimeError(
            f"authoring websocket verdict exceeds {MAX_WEBSOCKET_FRAME_BYTES} bytes"
        )
    return opcode, take(size)


def authoring_socket_path(control_root, interface):
    digest = hashlib.sha256(interface.encode("utf-8")).hexdigest()
    return os.path.join(control_root, f"{digest[:24]}.sock")


def send_authoring_manifest(schedule, manifest_path):
    manifest_bytes = read_regular_file_bounded(
        manifest_path,
        MAX_AUTHORING_MANIFEST_BYTES,
        "cron authoring manifest",
    )
    try:
        manifest = json.loads(manifest_bytes)
    except json.JSONDecodeError as error:
        raise RuntimeError(f"authoring manifest {manifest_path!r} is malformed") from error
    if not isinstance(manifest, dict):
        raise RuntimeError("authoring manifest must be a JSON object")
    payload = json.dumps(manifest, separators=(",", ":"), sort_keys=True).encode("utf-8")
    timeout = schedule["authoring"]["result_timeout_ms"] / 1000.0
    socket_path = authoring_socket_path(
        root_slots()["app_server_control_root"],
        schedule["authoring"]["source_interface"],
    )
    key = base64.b64encode(os.urandom(16)).decode("ascii")
    request = (
        "GET /rpc HTTP/1.1\r\n"
        "Host: localhost\r\n"
        "Upgrade: websocket\r\n"
        "Connection: Upgrade\r\n"
        f"Sec-WebSocket-Key: {key}\r\n"
        "Sec-WebSocket-Version: 13\r\n"
        "\r\n"
    ).encode("ascii")
    sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    sock.settimeout(timeout)
    try:
        sock.connect(socket_path)
        sock.sendall(request)
        buffered = read_http_upgrade(sock, key)
        sock.sendall(websocket_frame(0x1, payload))
        while True:
            opcode, reply = websocket_read_frame(sock, buffered)
            if opcode == 0x1:
                break
            if opcode == 0x8:
                raise RuntimeError("authoring websocket closed before a delivery verdict")
            if opcode == 0x9:
                sock.sendall(websocket_frame(0xA, reply))
                continue
            if opcode == 0xA:
                continue
            raise RuntimeError(f"authoring websocket returned unsupported opcode {opcode}")
        try:
            verdict = json.loads(reply.decode("utf-8", "strict"))
        except (UnicodeDecodeError, json.JSONDecodeError) as error:
            raise RuntimeError("authoring websocket returned a malformed verdict") from error
        if not isinstance(verdict, dict) or len(verdict) != 1:
            raise RuntimeError(f"authoring websocket returned an invalid verdict: {verdict!r}")
        admitted = verdict.get("admitted")
        if isinstance(admitted, dict) and isinstance(admitted.get("seq"), int):
            sock.sendall(websocket_frame(0x8, b""))
            return verdict
        refused = verdict.get("refused")
        if (
            isinstance(refused, dict)
            and isinstance(refused.get("reason"), str)
            and refused["reason"]
            and "detail" in refused
        ):
            sock.sendall(websocket_frame(0x8, b""))
            return verdict
        raise RuntimeError(f"authoring websocket returned an invalid verdict: {verdict!r}")
    finally:
        sock.close()


def find_admitted_authoring_seq(schedule, artifact_id):
    for line in _reverse_complete_ledger_lines(_ledger_path()):
        try:
            movement = json.loads(line)
        except (UnicodeDecodeError, json.JSONDecodeError) as error:
            raise RuntimeError("cron ledger contains a malformed complete row") from error
        if (
            not isinstance(movement, dict)
            or movement.get("kind") != "InterfaceArrival"
            or movement.get("from")
            != {"Interface": schedule["authoring"]["source_interface"]}
        ):
            continue
        artifact = movement.get("artifact")
        if not isinstance(artifact, dict) or artifact.get("id") != artifact_id:
            continue
        metadata = movement.get("invocation_metadata")
        runtime = metadata.get("runtime") if isinstance(metadata, dict) else None
        if not isinstance(runtime, dict) or runtime.get("run") != schedule["run"]:
            continue
        sequence = movement.get("seq")
        if isinstance(sequence, bool) or not isinstance(sequence, int) or sequence < 0:
            raise RuntimeError("cron admitted authoring movement has an invalid seq")
        return sequence
    return None


def send_authoring_manifest_idempotent(schedule, manifest_path, artifact_id):
    admitted = find_admitted_authoring_seq(schedule, artifact_id)
    if admitted is not None:
        return {"admitted": {"seq": admitted}}
    verdict = send_authoring_manifest(schedule, manifest_path)
    if "admitted" in verdict:
        return verdict
    refused = verdict.get("refused")
    detail = refused.get("detail") if isinstance(refused, dict) else None
    if (
        isinstance(detail, dict)
        and detail.get("artifact_id") == artifact_id
        and detail.get("message")
        == "arrival evidence already exists in the movement ledger"
    ):
        admitted = find_admitted_authoring_seq(schedule, artifact_id)
        if admitted is not None:
            return {"admitted": {"seq": admitted}}
    raise DeliveryRefused(verdict)


def checkpoint_record(run, checkpoint, inbound, outbound, artifacts, required):
    return {
        "id": checkpoint,
        "run": run,
        "trace": f"trace:{run}:{checkpoint}",
        "epoch": 0,
        "interface_binding": authored_interface_binding(inbound),
        "outbound_interface_bindings": [
            authored_interface_binding(interface) for interface in outbound
        ],
        "artifact_bindings": json.loads(json.dumps(artifacts)),
        "required_verified_artifacts": json.loads(json.dumps(required)),
    }


def _ledger_path():
    path = os.environ.get("OCTOPUS_MOVEMENTS_JSONL")
    if not isinstance(path, str) or not path or not os.path.isabs(path):
        raise RuntimeError("carry_forward requires an absolute OCTOPUS_MOVEMENTS_JSONL")
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


def _ledger_tail_watermark(path):
    for line in _reverse_complete_ledger_lines(path):
        try:
            movement = json.loads(line)
        except (UnicodeDecodeError, json.JSONDecodeError) as error:
            raise RuntimeError("carry_forward ledger tail is malformed") from error
        if not isinstance(movement, dict):
            raise RuntimeError("carry_forward ledger tail must be a JSON object")
        seq = movement.get("seq")
        if isinstance(seq, bool) or not isinstance(seq, int) or seq < 0:
            raise RuntimeError("carry_forward ledger tail has an invalid seq")
        return seq + 1
    return 0


def _movement_carry_forward_artifact(movement, schedule):
    if not isinstance(movement, dict):
        raise RuntimeError("carry_forward ledger movement must be a JSON object")
    selector = schedule["carry_forward"]
    if movement.get("kind") != {"MarkOutcomeAdmitted": selector["mark"]}:
        return None
    metadata = movement.get("invocation_metadata")
    runtime = metadata.get("runtime") if isinstance(metadata, dict) else None
    if not isinstance(runtime, dict) or runtime.get("run") != schedule["run"]:
        return None
    artifact = movement.get("artifact")
    artifact_id = artifact.get("id") if isinstance(artifact, dict) else None
    if not isinstance(artifact_id, str) or not artifact_id.strip():
        raise RuntimeError("carry_forward admitted mark has no artifact id")
    identity = artifact_id.rsplit(":", 1)
    if (
        len(identity) != 2
        or identity[0] != selector["artifact_label"]
        or re.fullmatch(r"[0-9a-f]{64}", identity[1]) is None
    ):
        return None
    return artifact_id


def _latest_carry_forward_from_ledger(schedule, path):
    for line in _reverse_complete_ledger_lines(path):
        try:
            movement = json.loads(line)
        except (UnicodeDecodeError, json.JSONDecodeError) as error:
            raise RuntimeError("carry_forward ledger contains a malformed complete row") from error
        artifact_id = _movement_carry_forward_artifact(movement, schedule)
        if artifact_id is not None:
            return artifact_id
    return None


def _latest_carry_forward_from_index(schedule, index_path, ledger_path):
    if not isinstance(index_path, str) or not index_path or not os.path.isabs(index_path):
        raise RuntimeError("OCTOPUS_LEDGER_INDEX_SQLITE is absent or not absolute")
    uri = f"file:{index_path}?mode=ro"
    try:
        connection = sqlite3.connect(uri, uri=True, timeout=0.25)
        try:
            schema = connection.execute(
                "SELECT value FROM index_meta WHERE key = 'schema_version'"
            ).fetchone()
            if schema != ("3",):
                raise RuntimeError("carry_forward ledger index has an unsupported schema version")
            watermark_row = connection.execute(
                "SELECT value FROM index_meta WHERE key = 'watermark'"
            ).fetchone()
            if watermark_row is None or len(watermark_row) != 1:
                raise RuntimeError("carry_forward ledger index has no watermark")
            try:
                watermark = int(watermark_row[0])
            except (TypeError, ValueError) as error:
                raise RuntimeError("carry_forward ledger index watermark is invalid") from error
            ledger_watermark = _ledger_tail_watermark(ledger_path)
            if watermark != ledger_watermark:
                raise RuntimeError(
                    f"carry_forward ledger index watermark {watermark} is behind ledger truth {ledger_watermark}"
                )
            selector = schedule["carry_forward"]
            row = connection.execute(
                "SELECT artifact_id FROM movements "
                "WHERE kind = ? AND mark = ? AND artifact_label = ? AND runtime_run = ? "
                "ORDER BY seq DESC LIMIT 1",
                (
                    "MarkOutcomeAdmitted",
                    selector["mark"],
                    selector["artifact_label"],
                    schedule["run"],
                ),
            ).fetchone()
        finally:
            connection.close()
    except sqlite3.Error as error:
        raise RuntimeError(f"carry_forward ledger index query failed: {error}") from error
    if row is None:
        return None
    if len(row) != 1 or not isinstance(row[0], str) or not row[0].strip():
        raise RuntimeError("carry_forward ledger index returned an invalid artifact id")
    identity = row[0].rsplit(":", 1)
    if (
        len(identity) != 2
        or identity[0] != schedule["carry_forward"]["artifact_label"]
        or re.fullmatch(r"[0-9a-f]{64}", identity[1]) is None
    ):
        raise RuntimeError("carry_forward ledger index returned a mismatched artifact label")
    return row[0]


def latest_carry_forward_artifact(schedule):
    global _CARRY_FORWARD_INDEX_WARNING
    if schedule["carry_forward"] is None:
        return None
    ledger_path = _ledger_path()
    try:
        return _latest_carry_forward_from_index(
            schedule,
            os.environ.get("OCTOPUS_LEDGER_INDEX_SQLITE"),
            ledger_path,
        )
    except (OSError, RuntimeError, ValueError, sqlite3.DatabaseError) as error:
        message = str(error)
        if _CARRY_FORWARD_INDEX_WARNING != message:
            sys.stderr.write(
                f"cron ledger index unavailable; reading ledger truth: {message}\n"
            )
            sys.stderr.flush()
            _CARRY_FORWARD_INDEX_WARNING = message
        return _latest_carry_forward_from_ledger(schedule, ledger_path)


def _frontier_has_arrival_from_index(schedule, checkpoint, index_path, ledger_path):
    if not isinstance(index_path, str) or not index_path or not os.path.isabs(index_path):
        raise RuntimeError("OCTOPUS_LEDGER_INDEX_SQLITE is absent or not absolute")
    uri = f"file:{index_path}?mode=ro"
    try:
        connection = sqlite3.connect(uri, uri=True, timeout=0.25)
        try:
            schema = connection.execute(
                "SELECT value FROM index_meta WHERE key = 'schema_version'"
            ).fetchone()
            if schema != ("3",):
                raise RuntimeError("cron ledger index has an unsupported schema version")
            watermark_row = connection.execute(
                "SELECT value FROM index_meta WHERE key = 'watermark'"
            ).fetchone()
            if watermark_row is None or len(watermark_row) != 1:
                raise RuntimeError("cron ledger index has no watermark")
            try:
                watermark = int(watermark_row[0])
            except (TypeError, ValueError) as error:
                raise RuntimeError("cron ledger index watermark is invalid") from error
            ledger_watermark = _ledger_tail_watermark(ledger_path)
            if watermark != ledger_watermark:
                raise RuntimeError(
                    f"cron ledger index watermark {watermark} is behind ledger truth {ledger_watermark}"
                )
            row = connection.execute(
                "SELECT 1 FROM movements "
                "WHERE kind = 'InterfaceArrival' AND from_kind = 'interface' "
                "AND from_id = ? AND runtime_run = ? AND runtime_checkpoint = ? "
                "ORDER BY seq DESC LIMIT 1",
                (INTERFACE_ID, schedule["run"], checkpoint),
            ).fetchone()
        finally:
            connection.close()
    except sqlite3.Error as error:
        raise RuntimeError(f"cron ledger index query failed: {error}") from error
    return row == (1,)


def _movement_is_frontier_arrival(movement, schedule, checkpoint):
    if not isinstance(movement, dict):
        raise RuntimeError("cron ledger movement must be a JSON object")
    if movement.get("kind") != "InterfaceArrival":
        return False
    if movement.get("from") != {"Interface": INTERFACE_ID}:
        return False
    metadata = movement.get("invocation_metadata")
    runtime = metadata.get("runtime") if isinstance(metadata, dict) else None
    return (
        isinstance(runtime, dict)
        and runtime.get("run") == schedule["run"]
        and runtime.get("checkpoint") == checkpoint
    )


def _frontier_has_arrival_from_ledger(schedule, checkpoint, ledger_path):
    for line in _reverse_complete_ledger_lines(ledger_path):
        try:
            movement = json.loads(line)
        except (UnicodeDecodeError, json.JSONDecodeError) as error:
            raise RuntimeError("cron ledger contains a malformed complete row") from error
        if _movement_is_frontier_arrival(movement, schedule, checkpoint):
            return True
    return False


def frontier_has_arrival(schedule, checkpoint):
    global _FRONTIER_INDEX_WARNING
    ledger_path = _ledger_path()
    try:
        return _frontier_has_arrival_from_index(
            schedule,
            checkpoint,
            os.environ.get("OCTOPUS_LEDGER_INDEX_SQLITE"),
            ledger_path,
        )
    except (OSError, RuntimeError, ValueError, sqlite3.DatabaseError) as error:
        message = str(error)
        if _FRONTIER_INDEX_WARNING != message:
            sys.stderr.write(
                f"cron ledger index unavailable; reading ledger truth: {message}\n"
            )
            sys.stderr.flush()
            _FRONTIER_INDEX_WARNING = message
        return _frontier_has_arrival_from_ledger(schedule, checkpoint, ledger_path)


def author_next_frontier(schedule, current):
    try:
        record = json.loads(current)
    except json.JSONDecodeError as error:
        raise RuntimeError("scheduled run record is malformed") from error
    if not isinstance(record, dict):
        raise RuntimeError("scheduled run record must be a JSON object")
    template = schedule["topology"]
    run = schedule["run"]
    if record.get("id") != run or record.get("worker") != template["worker"]:
        raise RuntimeError("scheduled run record identity drifted from its declared run template")
    states = record.get("interface_state")
    if not isinstance(states, dict):
        raise RuntimeError("scheduled run interface_state must be an object")
    state = states.get(INTERFACE_ID)
    if not isinstance(state, dict):
        raise RuntimeError(f"scheduled run has no interface_state for {INTERFACE_ID!r}")
    generation = state.get("authoring_generation")
    frontier = state.get("frontier_checkpoint_ids")
    if isinstance(generation, bool) or not isinstance(generation, int) or generation < 0:
        raise RuntimeError("scheduled run authoring_generation must be unsigned")
    if (
        not isinstance(frontier, list)
        or len(frontier) != 3
        or any(not isinstance(value, str) or not value for value in frontier)
        or len(set(frontier)) != len(frontier)
    ):
        raise RuntimeError("scheduled run frontier_checkpoint_ids must contain three unique ids")
    checkpoints = record.get("checkpoints")
    active = record.get("active_checkpoints")
    if not isinstance(checkpoints, list) or not isinstance(active, list):
        raise RuntimeError("scheduled run checkpoints and active_checkpoints must be arrays")
    if any(not isinstance(checkpoint, dict) for checkpoint in checkpoints):
        raise RuntimeError("scheduled run checkpoints contain a non-object")
    if any(not isinstance(checkpoint, str) or not checkpoint for checkpoint in active):
        raise RuntimeError("scheduled run active_checkpoints contain an invalid id")
    checkpoint_ids = [
        checkpoint.get("id") for checkpoint in checkpoints
    ]
    if any(checkpoint_ids.count(checkpoint) != 1 for checkpoint in frontier):
        raise RuntimeError("scheduled run frontier does not own exactly one checkpoint per id")
    if any(active.count(checkpoint) != 1 for checkpoint in frontier):
        raise RuntimeError("scheduled run frontier is not exactly active")
    next_generation = generation + 1
    trigger_id = f"{template['checkpoint']}_{next_generation}"
    call_id = f"{template['checkpoint']}_authoring_call_{next_generation}"
    result_id = f"{template['checkpoint']}_authoring_result_{next_generation}"
    next_ids = [trigger_id, call_id, result_id]
    if any(checkpoint in checkpoint_ids or checkpoint in active for checkpoint in next_ids):
        raise RuntimeError("scheduled run next frontier checkpoint id already exists")
    artifacts = json.loads(json.dumps(template.get("artifact_bindings", [])))
    carry_forward = latest_carry_forward_artifact(schedule)
    if carry_forward is not None:
        existing_artifacts = {
            binding.get("id")
            for binding in artifacts
            if isinstance(binding, dict) and isinstance(binding.get("id"), str)
        }
        if carry_forward not in existing_artifacts:
            artifacts.append({"id": carry_forward})
    required = template.get("required_verified_artifacts", [])
    next_checkpoints = [
        checkpoint_record(
            run,
            trigger_id,
            INTERFACE_ID,
            template.get("outbound_bindings", []),
            artifacts,
            required,
        ),
        checkpoint_record(
            run,
            call_id,
            schedule["authoring"]["source_interface"],
            [schedule["authoring"]["environment_interface"]],
            [],
            [],
        ),
        checkpoint_record(
            run,
            result_id,
            schedule["authoring"]["environment_interface"],
            [],
            [],
            [],
        ),
    ]
    frontier_set = set(frontier)
    record["checkpoints"] = [
        checkpoint
        for checkpoint in checkpoints
        if checkpoint.get("id") not in frontier_set
    ] + next_checkpoints
    record["active_checkpoints"] = [
        checkpoint for checkpoint in active if checkpoint not in frontier_set
    ] + next_ids
    state["authoring_generation"] = next_generation
    state["frontier_checkpoint_ids"] = next_ids
    content = json.dumps(record, indent=2, sort_keys=True) + "\n"
    return content.encode("utf-8"), next_generation


def ensure_scheduled_run(schedule):
    run_path = os.path.join(root_slots()["run_records_root"], f"{schedule['run']}.json")
    try:
        existing = read_regular_file_bounded(
            run_path,
            MAX_RUN_RECORD_BYTES,
            "scheduled run record",
        )
    except FileNotFoundError as error:
        raise RuntimeError(
            f"scheduled run record {run_path!r} is an operator-authored prerequisite"
        ) from error
    try:
        record = json.loads(existing)
    except json.JSONDecodeError as error:
        raise RuntimeError(f"scheduled run record {run_path!r} is malformed") from error
    if not isinstance(record, dict):
        raise RuntimeError(f"scheduled run record {run_path!r} is not a JSON object")
    template = schedule["run_template"]
    if record.get("id") != schedule["run"] or record.get("worker") != template["worker"]:
        raise RuntimeError(
            f"scheduled run record {run_path!r} identity conflicts with the declared run_template"
        )
    scheduled_topology(record, schedule["run"], schedule["authoring"])
    return record["interface_state"][INTERFACE_ID]["frontier_checkpoint_ids"][0]


def rearm_scheduled_run(schedule):
    run_path = os.path.join(root_slots()["run_records_root"], f"{schedule['run']}.json")
    current = read_regular_file_bounded(
        run_path,
        MAX_RUN_RECORD_BYTES,
        "scheduled run record",
    )
    authored, generation = author_next_frontier(schedule, current)
    try:
        current_text = current.decode("utf-8", "strict")
    except UnicodeDecodeError as error:
        raise RuntimeError("scheduled run record is not UTF-8") from error
    correlation = f"scheduled-run-authoring:{INTERFACE_ID}:{schedule['run']}:{generation}"
    payload = json.dumps(
        {
            "tool": schedule["authoring"]["file_write_tool"],
            "arguments": {
                "path": run_path,
                "content": authored.decode("utf-8"),
                "expected_before_content": current_text,
            },
            "correlation_id": correlation,
        },
        separators=(",", ":"),
        sort_keys=True,
    ).encode("utf-8")
    authoring_root = os.path.join(
        root_slots()["interface_hand_in_root"],
        schedule["authoring"]["source_interface"],
    )
    manifest_path, _ = materialize_artifact(
        schedule["authoring"]["file_write_label"], payload, authoring_root
    )
    verdict = send_authoring_manifest(schedule, manifest_path)
    if "refused" in verdict:
        return verdict
    deadline = time.monotonic() + schedule["authoring"]["result_timeout_ms"] / 1000.0
    while True:
        try:
            observed = read_regular_file_bounded(
                run_path,
                MAX_RUN_RECORD_BYTES,
                "scheduled run record",
            )
        except FileNotFoundError:
            observed = None
        if observed == authored:
            return verdict
        if observed not in (None, current):
            raise RuntimeError("scheduled run changed concurrently during interface authoring")
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise RuntimeError(
                f"scheduled run file.write did not materialize within {schedule['authoring']['result_timeout_ms']}ms"
            )
        time.sleep(min(0.02, remaining))


def prepare_new_run_authoring_attempt(schedule, task_state, writes):
    run_path = os.path.join(root_slots()["run_records_root"], f"{schedule['run']}.json")
    current = read_regular_file_bounded(
        run_path,
        MAX_RUN_RECORD_BYTES,
        "scheduled controller run record",
    )
    authored, generation = author_next_frontier(schedule, current)
    try:
        current_text = current.decode("utf-8", "strict")
        authored_text = authored.decode("utf-8", "strict")
    except UnicodeDecodeError as error:
        raise RuntimeError("scheduled controller run record is not UTF-8") from error
    batch = [
        {
            "path": run_path,
            "content": authored_text,
            "expected_before_content": current_text,
        },
        *writes,
    ]
    correlation = (
        f"scheduled-new-run-authoring:{INTERFACE_ID}:"
        f"{task_state['manifest_hash']}:{generation}"
    )
    payload = json.dumps(
        {
            "tool": schedule["authoring"]["file_write_tool"],
            "arguments": {"writes": batch},
            "correlation_id": correlation,
        },
        separators=(",", ":"),
        sort_keys=True,
    ).encode("utf-8")
    authoring_root = os.path.join(
        root_slots()["interface_hand_in_root"],
        schedule["authoring"]["source_interface"],
    )
    manifest_path, manifest_hash = materialize_artifact(
        schedule["authoring"]["file_write_label"],
        payload,
        authoring_root,
    )
    task_state["authoring_attempt"] = {
        "before_hash": octopus_content_hash(current),
        "after_hash": octopus_content_hash(authored),
        "generation": generation,
        "manifest_path": manifest_path,
        "artifact_id": (
            f"{schedule['authoring']['file_write_label']}:"
            f"{manifest_hash}"
        ),
    }
    commit_task_plan_state(schedule, task_state)


def observe_new_run_authoring(run_path, attempt, writes):
    try:
        controller = read_regular_file_bounded(
            run_path,
            MAX_RUN_RECORD_BYTES,
            "scheduled controller run record",
        )
    except FileNotFoundError:
        controller = None
    controller_hash = (
        octopus_content_hash(controller) if controller is not None else None
    )
    settled = controller_hash == attempt["after_hash"] and task_writes_complete(writes)
    return controller_hash, settled


def finish_new_run_authoring_attempt(schedule, task_state, writes):
    validate_task_plan(schedule, task_state["manifest_hash"], task_state)
    attempt = task_state["authoring_attempt"]
    if attempt is None:
        raise RuntimeError("scheduled new-run authoring attempt is absent")
    send_authoring_manifest_idempotent(
        schedule,
        attempt["manifest_path"],
        attempt["artifact_id"],
    )
    run_path = os.path.join(root_slots()["run_records_root"], f"{schedule['run']}.json")
    deadline = time.monotonic() + schedule["authoring"]["result_timeout_ms"] / 1000.0
    while True:
        try:
            observed = observe_new_run_authoring(run_path, attempt, writes)
        except FileChangedDuringRead:
            observed = None
        if observed is not None:
            controller_hash, settled = observed
            if settled:
                task_state["controller_frontier_hashes"] = [attempt["after_hash"]]
                task_state["authoring_attempt"] = None
                commit_task_plan_state(schedule, task_state)
                return
            if controller_hash not in (
                None,
                attempt["before_hash"],
                attempt["after_hash"],
            ):
                raise RuntimeError(
                    "scheduled controller run changed concurrently during new-run authoring"
                )
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise RuntimeError(
                "scheduled new-run file.write did not materialize within "
                f"{schedule['authoring']['result_timeout_ms']}ms"
            )
        time.sleep(min(0.02, remaining))


def ensure_new_run_target(schedule, manifest_hash, label):
    task_state = load_or_prepare_task_plan(schedule, manifest_hash, label)
    writes = task_plan_writes(schedule, task_state["plan"])
    run_path = os.path.join(root_slots()["run_records_root"], f"{schedule['run']}.json")
    current = read_regular_file_bounded(
        run_path,
        MAX_RUN_RECORD_BYTES,
        "scheduled controller run record",
    )
    current_hash = octopus_content_hash(current)
    if (
        task_writes_complete(writes)
        and current_hash in task_state["controller_frontier_hashes"]
    ):
        return
    if task_state["authoring_attempt"] is not None:
        manifest = load_json_object(task_state["authoring_attempt"]["manifest_path"], "pending task authoring manifest", MAX_AUTHORING_MANIFEST_BYTES, private=True)
        payload = read_regular_file_bounded(manifest["payload_path"], HOST_IO_LIMITS["max_payload_bytes"], "pending task authoring payload", private=True)
        if octopus_content_hash(payload) != manifest["payload_content_hash"]:
            raise RuntimeError("pending task authoring payload changed")
        pending = json.loads(payload)["arguments"]["writes"][1:]
        planned = {write["path"]: write for write in writes}
        if not pending or any(write != planned.get(write.get("path")) for write in pending):
            raise RuntimeError("pending authoring batch differs from its task plan")
        finish_new_run_authoring_attempt(schedule, task_state, pending)
    size = schedule["authoring"].get("max_files_per_call", 8)
    for start in range(0, len(writes), size):
        batch = writes[start:start + size]
        if task_state["controller_frontier_hashes"] and task_writes_complete(batch):
            continue
        prepare_new_run_authoring_attempt(schedule, task_state, batch)
        finish_new_run_authoring_attempt(schedule, task_state, batch)


def resolve_reminder(schedule):
    env_name = schedule["reminder_env"]
    value = os.environ.get(env_name, "").strip() if env_name else ""
    return value or schedule["reminder_default"]


def now(schedule):
    return datetime.now(schedule["timezone"])


def ensure_time_frontier(schedule):
    frontier = ensure_scheduled_run(schedule)
    rearm_verdict = (
        rearm_scheduled_run(schedule)
        if frontier_has_arrival(schedule, frontier)
        else None
    )
    if isinstance(rearm_verdict, dict) and "refused" in rearm_verdict:
        raise DeliveryRefused(rearm_verdict)


def time_artifact_label(schedule, payload):
    if schedule["target"]["mode"] == "new_run":
        return f"{TIME_LABEL}.{octopus_content_hash(payload)[:24]}"
    return TIME_LABEL


def checked_time_artifact_label(label, context):
    if label != TIME_LABEL and (not isinstance(label, str) or TASK_TIME_LABEL.fullmatch(label) is None):
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
    label = time_artifact_label(schedule, payload)
    manifest_path, manifest_hash = materialize_artifact(label, payload)
    return label, manifest_path, manifest_hash


def deliver_time_artifact(schedule, label, manifest_path, manifest_hash):
    if schedule["target"]["mode"] == "new_run":
        artifact_id, _ = arrival_identity(label, manifest_hash)
        admitted = find_admitted_arrival_seq(artifact_id)
        if admitted is not None:
            return admitted
        ensure_new_run_target(schedule, manifest_hash, label)
        return hand_artifact_idempotent(manifest_path, label, manifest_hash)
    ensure_time_frontier(schedule)
    try:
        return hand_artifact_idempotent(manifest_path, label, manifest_hash)
    except DeliveryRefused as error:
        refused = error.verdict.get("refused")
        if not isinstance(refused, dict) or refused.get("reason") != "checkpoint_spent":
            raise
    ensure_time_frontier(schedule)
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


def deliver_one_shot_time_artifact(schedule, label, manifest_path, manifest_hash):
    artifact_id, _ = arrival_identity(label, manifest_hash)
    admitted = find_admitted_arrival_seq(artifact_id)
    if admitted is not None:
        return admitted
    if schedule["target"]["mode"] == "new_run":
        ensure_new_run_target(schedule, manifest_hash, label)
    else:
        frontier = ensure_scheduled_run(schedule)
        if frontier_has_arrival(schedule, frontier):
            raise RuntimeError("one-shot target checkpoint is spent; an explicit projected binding is required")
    return hand_artifact_idempotent(manifest_path, label, manifest_hash)


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
    sequence = deliver_one_shot_time_artifact(schedule, label, manifest_path, manifest_hash)
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
            if cron_due(compiled, minute, minute_before(minute)) and minute_key != last_minute:
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
        if cron_due(compiled, minute, minute_before(minute)) and minute_key != last_minute:
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
        owned_runs = {self.schedule["run"]}
        if os.path.exists(RUN_PLANS_DIR):
            for entry in os.scandir(RUN_PLANS_DIR):
                if not entry.name.endswith(".json"):
                    continue
                plan = self.document(entry.path)
                seen.add(entry.path)
                run_id = plan.get("plan", {}).get("run_id")
                if plan.get("schema_version") != 1 or not isinstance(run_id, str) or REGISTRY_ID.fullmatch(run_id) is None:
                    raise ValueError("inactivity target plan has invalid run provenance")
                owned_runs.add(run_id)
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
                excluded = INTERFACE_ID in interfaces or run["id"] in owned_runs
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
                    if self.source_matches(source, condition.get("exclude_interface_match"), self.excluded_sources):
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
        if self.schedule["target"]["mode"] == "new_run":
            ensure_new_run_target(self.schedule, manifest_hash, label)
        else:
            ensure_time_frontier(self.schedule)
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
