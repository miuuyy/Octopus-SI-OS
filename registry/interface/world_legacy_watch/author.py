#!/usr/bin/env python3
import argparse
import datetime
import json
import os
import re
import secrets
import shutil
import subprocess
import sys

sys.dont_write_bytecode = True

KIT_DIR = os.path.dirname(os.path.abspath(__file__))
KIT_INTERFACES = os.path.dirname(KIT_DIR)
sys.path.insert(0, KIT_DIR)

from octopus_content_hash import octopus_content_hash

SENSOR_ID = "world_legacy_watch"
RELAY_WORKER_ID = "system_worker_legacy_relay"
RELAY_RUN_ID = "run_legacy_watch_relay"
RELAY_CHECKPOINT_ID = "cp_legacy_watch_email"
EMAIL_ID = "world_email"
EVENT_LABEL = "email.event"
TASK_WORKER_ID = "system_worker_operator_task"
TASK_CHECKPOINT_ID = "cp_legacy_watch"
DEFAULT_TITLE = "Legacy watch"
ARMED_FILE = "armed.json"
STAGING_PREFIX = ".legacy-watch-staging-"
CHAIN_INTERFACES = (EMAIL_ID, SENSOR_ID, RELAY_WORKER_ID)
KIT_FILES = {
    SENSOR_ID: ["interface.json", "life.json", "legacy_watch_life.py", "octopus_content_hash.py", "watch.json"],
    RELAY_WORKER_ID: ["interface.json", "policy.json"],
}
APP_BIN = os.path.join("Contents", "Resources", "app", "bin")
PLANNER_NAME = "octopus-operator-chat-topology"
RUNTIME_NAME = "octopus"
APPLICATION_CATEGORIES = ("connector", "plugin", "artifact")
HOST_FILESYSTEM_ROOT = "host_filesystem_root"
WORKSPACE_ROOT = "workspace_root"
PROFILE_FIELDS = (
    "external_inputs", "external_relays", "model_outputs", "context_artifact_bindings",
    "payload_artifact_bindings", "selected_worker_environment_kinds", "selected_tools",
)
PROFILE_KEYS = {"ee_id", "worker_choice_interface", "door_host_slots", "publication_evidence", *PROFILE_FIELDS}
CHECKPOINT_PATTERN = re.compile(r"^[A-Za-z0-9_-]{1,96}$")
PLAN_REQUIRED = {
    "run_id", "chat_interface_id", "model_interface_id", "worker_interface_id", "client_surface_role",
    "expected_before_hash_algorithm", "interface_writes", "run_writes", "removals", "run_record_content",
}
PLAN_ALLOWED = PLAN_REQUIRED | {
    "authoring_source_interface_id", "authoring_environment_interface_id", "reasoning_effort",
    "ee_template_chain", "capability_doors", "capability_tools", "authoring_tool_id", "worker_relays",
    "model", "service_tier", "authoring_profile", "required_interface_ids", "required_tool_ids",
    "publication_evidence", "publication_expansion",
}


class Refusal(Exception):
    pass


class Undo:
    def __init__(self):
        self.steps = []

    def add(self, description, action):
        self.steps.append((description, action))

    def run(self):
        failures = []
        for description, action in reversed(self.steps):
            try:
                action()
                say(f"undid {description}")
            except Exception as error:
                failures.append(f"{description}: {error}")
        self.steps = []
        return failures


def say(message):
    print(message, flush=True)


def read_json(path, name):
    try:
        with open(path, "rb") as handle:
            return json.loads(handle.read())
    except FileNotFoundError:
        raise Refusal(f"{name} is missing: {path}") from None
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise Refusal(f"{name} is not valid JSON ({error}): {path}") from None


def require(condition, message):
    if not condition:
        raise Refusal(message)


def pretty(value):
    return json.dumps(value, ensure_ascii=False, indent=2) + "\n"


def sync_directory(path):
    descriptor = os.open(path, os.O_RDONLY)
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def temporary_beside(path):
    return os.path.join(os.path.dirname(path), f".{os.path.basename(path)}.{os.getpid()}.{secrets.token_hex(4)}.tmp")


def write_temporary(path, content, mode):
    temporary = temporary_beside(path)
    descriptor = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_NOFOLLOW", 0), mode)
    with os.fdopen(descriptor, "wb") as handle:
        handle.write(content)
        handle.flush()
        os.fsync(handle.fileno())
    os.chmod(temporary, mode)
    return temporary


def missing_directories(directory):
    missing = []
    while not os.path.lexists(directory):
        missing.append(directory)
        directory = os.path.dirname(directory)
    return missing


def remove_created(path, content, directories):
    with open(path, "rb") as handle:
        if handle.read() != content:
            raise Refusal(f"{path} changed after it was written; it is left in place")
    os.unlink(path)
    for directory in directories:
        os.rmdir(directory)


def restore_replaced(path, previous, written):
    with open(path, "rb") as handle:
        if handle.read() != written:
            raise Refusal(f"{path} changed after it was written; it is left in place")
    replace_file(path, previous, octopus_content_hash(written))


def create_file(path, content, undo, mode=0o644):
    directories = missing_directories(os.path.dirname(path))
    os.makedirs(os.path.dirname(path), exist_ok=True)
    temporary = write_temporary(path, content, mode)
    try:
        os.link(temporary, path)
    except FileExistsError:
        raise Refusal(f"{path} appeared while authoring; nothing overwrites it") from None
    finally:
        os.unlink(temporary)
    sync_directory(os.path.dirname(path))
    undo.add(f"created {path}", lambda: remove_created(path, content, directories))


def replace_file(path, content, expected_hash):
    with open(path, "rb") as handle:
        current = handle.read()
    if octopus_content_hash(current) != expected_hash:
        raise Refusal(f"{path} changed after it was planned; nothing was overwritten")
    mode = os.stat(path).st_mode & 0o777
    temporary = write_temporary(path, content, mode)
    try:
        os.replace(temporary, path)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)
    sync_directory(os.path.dirname(path))


def inside(path, root):
    return os.path.commonpath([os.path.realpath(root), path]) == os.path.realpath(root)


def interface_json(registry, interface_id):
    path = os.path.join(registry, "interface", interface_id, "interface.json")
    descriptor = read_json(path, f"interface {interface_id}")
    require(isinstance(descriptor, dict) and descriptor.get("id") == interface_id, f"{path} does not declare id {interface_id}")
    return descriptor


def home_layout(home):
    require(os.path.isdir(home), f"{home} is not a directory")
    layout = {
        "home": home,
        "registry": os.path.join(home, "registry"),
        "interfaces": os.path.join(home, "registry", "interface"),
        "runs": os.path.join(home, "state", "runs"),
        "ee_instances": os.path.join(home, "state", "ee_instances"),
        "private_state": os.path.join(home, "interfaces"),
    }
    for key in ("interfaces", "runs", "ee_instances"):
        require(os.path.isdir(layout[key]), f"this Home is not set up: {layout[key]} is missing")
    return layout


def run_records(layout):
    records = {}
    for name in sorted(os.listdir(layout["runs"])):
        if not name.endswith(".json") or name.startswith("."):
            continue
        path = os.path.join(layout["runs"], name)
        record = read_json(path, "run record")
        require(isinstance(record, dict) and record.get("id") == name[:-5], f"run record {path} does not name itself {name[:-5]}")
        records[record["id"]] = record
    return records


def home_run(layout, records):
    found = []
    for run_id, record in records.items():
        state = record.get("interface_state")
        if not isinstance(state, dict):
            continue
        for interface_id, value in state.items():
            if isinstance(value, dict) and value.get("client_surface_role") == "home":
                found.append((run_id, interface_id, value))
    require(found, "this Home has no Home run (no run's client interface state has client_surface_role home)")
    require(len(found) == 1, f"this Home has more than one Home run: {', '.join(row[0] for row in found)}")
    run_id, chat_interface, state = found[0]
    sidecar_path = os.path.join(layout["interfaces"], chat_interface, "octopus_os.json")
    sidecar = read_json(sidecar_path, "Home client sidecar")
    require(
        isinstance(sidecar, dict)
        and sidecar.get("workspace") == "octopus_os"
        and sidecar.get("component") == "client"
        and sidecar.get("surface_role") == "home"
        and sidecar.get("interface") == chat_interface,
        f"the Home client sidecar of {chat_interface} does not describe a Home client",
    )
    ee_id = records[run_id].get("ee")
    require(isinstance(ee_id, str) and ee_id, f"Home run {run_id} names no EE")
    instance = read_json(os.path.join(layout["ee_instances"], f"{ee_id}.json"), "root Existence EE instance")
    require(
        isinstance(instance, dict)
        and instance.get("ee_id") == ee_id
        and instance.get("template_name") == "existence"
        and instance.get("parent_ee_id") is None,
        f"Home run {run_id} does not live directly in a root Existence EE ({ee_id})",
    )
    provider = state.get("provider_choice_interface")
    runtime = state.get("provider_runtime_interface")
    require(isinstance(provider, str) and provider, f"Home run {run_id} records no provider_choice_interface")
    require(isinstance(runtime, str) and runtime, f"Home run {run_id} records no provider_runtime_interface")
    private = (interface_json(layout["registry"], runtime).get("interface_driver_config") or {}).get("private_config") or {}
    config = instance.get("config") or {}
    octopus_os = (config.get("octopus_os") if isinstance(config, dict) else None) or {}
    require(isinstance(config, dict) and isinstance(octopus_os, dict), f"root EE {ee_id} has a malformed config.octopus_os")
    return {
        "run_id": run_id,
        "ee_id": ee_id,
        "provider": provider,
        "model": private.get("model"),
        "effort": private.get("reasoning_effort"),
        "service_tier": private.get("service_tier"),
        "sidecar": sidecar,
        "sidecar_path": sidecar_path,
        "octopus_os": octopus_os,
    }


def task_profile(home):
    sidecar, path, ee_id = home["sidecar"], home["sidecar_path"], home["ee_id"]
    key = (ee_id, TASK_WORKER_ID)
    own = sidecar.get("authoring_profile")
    profiles = sidecar.get("task_authoring_profiles")
    require(profiles is None or isinstance(profiles, list), f"{path} task_authoring_profiles is not a list")
    if isinstance(own, dict) and (own.get("ee_id"), own.get("worker_choice_interface")) == key:
        matches = [own]
    else:
        matches = [row for row in profiles or [] if isinstance(row, dict) and (row.get("ee_id"), row.get("worker_choice_interface")) == key]
    require(
        matches,
        f"the Home client sidecar {path} has no task authoring profile for EE {ee_id} and worker {TASK_WORKER_ID}, "
        "so the client offers no ordinary task there; set the Home up through the client first",
    )
    require(len(matches) == 1, f"the Home client sidecar {path} repeats the task authoring profile for {ee_id} and {TASK_WORKER_ID}")
    profile = matches[0]
    unknown = set(profile) - PROFILE_KEYS
    require(not unknown, f"the task authoring profile in {path} has undeclared fields: {sorted(unknown)}")
    slots = profile.get("door_host_slots")
    require(
        isinstance(slots, dict) and all(isinstance(row, dict) and all(isinstance(root, str) and os.path.isabs(root) for root in row.values()) for row in slots.values()),
        f"the task authoring profile in {path} has malformed door_host_slots",
    )
    for field in PROFILE_FIELDS:
        rows = profile.get(field, [])
        require(isinstance(rows, list), f"the task authoring profile in {path} has a non-list {field}")
        if field != "selected_worker_environment_kinds":
            require(all(isinstance(row, dict) for row in rows), f"the task authoring profile in {path} has a malformed {field} entry")
    return profile


def formed_policy(runtime, layout, ee_id, provider, model):
    completed = subprocess.run(
        [runtime, "project-ee-policies"],
        env={**os.environ, "OCTOPUS_HOME": layout["home"]},
        capture_output=True, timeout=120, check=False,
    )
    if completed.returncode != 0:
        raise Refusal(f"the EE policy projection failed ({completed.returncode}): {completed.stderr.decode('utf-8', 'replace').strip()}")
    try:
        snapshot = json.loads(completed.stdout)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise Refusal(f"the EE policy projection returned malformed JSON: {error}") from None
    require(isinstance(snapshot, dict) and isinstance(snapshot.get("environments"), list), "the EE policy projection has no environments list")
    rows = [row for row in snapshot["environments"] if isinstance(row, dict) and row.get("ee_id") == ee_id]
    require(len(rows) == 1, f"the EE policy projection has no single row for {ee_id}")
    effective = rows[0].get("effective_policy") or {}
    axes = {}
    for name, field in (("interfaces", "allowed_interfaces"), ("tools", "allowed_tools"), ("kinds", "allowed_worker_environment_kinds")):
        values = effective.get(field)
        require(isinstance(values, list), f"the formed policy of {ee_id} has no {field} list")
        axes[name] = set(values)
    workers = {row.get("worker_interface_id"): row.get("interface_policy") for row in rows[0].get("workers") or [] if isinstance(row, dict)}
    require(TASK_WORKER_ID in workers, f"the formed policy of {ee_id} offers no worker {TASK_WORKER_ID}")
    models = ((workers[TASK_WORKER_ID] or {}).get(provider) or {}).get("models")
    require(
        provider in axes["interfaces"] and isinstance(models, list) and model in models,
        f"the formed policy of {TASK_WORKER_ID} in {ee_id} does not admit model {model!r} of {provider}",
    )
    return axes


def worker_ceiling(layout):
    path = os.path.join(layout["interfaces"], TASK_WORKER_ID, "policy.json")
    policy = read_json(path, f"{TASK_WORKER_ID} policy")
    ceiling = policy.get("ceiling") if isinstance(policy, dict) else None
    require(isinstance(ceiling, dict), f"{path} has no ceiling")
    result = {}
    for field in ("interfaces", "tools", "wek"):
        values = ceiling.get(field)
        require(isinstance(values, list) and all(isinstance(value, str) for value in values), f"{path} ceiling has no {field} list")
        result[field] = set(values)
    return result


def registry_tools(layout):
    root = os.path.join(layout["registry"], "tool")
    tools = {}
    for name in sorted(os.listdir(root)):
        path = os.path.join(root, name, "tool.json")
        if not os.path.isfile(path):
            continue
        descriptor = read_json(path, f"tool {name}")
        kinds = descriptor.get("supported_worker_environment_kinds") if isinstance(descriptor, dict) else None
        require(
            isinstance(descriptor, dict) and isinstance(descriptor.get("id"), str) and isinstance(descriptor.get("interface"), str) and isinstance(kinds, list),
            f"{path} is not a tool descriptor",
        )
        tools[descriptor["id"]] = (descriptor["interface"], set(kinds))
    return tools


def offered_drivers(layout, policy, ceiling, tools, octopus_os):
    root = os.path.join(layout["registry"], "worker_environment_kind")
    host_slots = octopus_os.get("driver_host_slots") or {}
    require(isinstance(host_slots, dict), "the root EE config.octopus_os.driver_host_slots is not an object")
    drivers = []
    for name in sorted(os.listdir(root)):
        path = os.path.join(root, name, "worker_environment_kind.json")
        if not os.path.isfile(path):
            continue
        descriptor = read_json(path, f"Worker Environment kind {name}")
        require(isinstance(descriptor, dict) and descriptor.get("worker_environment_kind_id") == name, f"{path} does not declare kind {name}")
        if name not in policy["kinds"] or name not in ceiling["wek"]:
            continue
        bindings = sorted(
            (tool, interface) for tool, (interface, kinds) in tools.items()
            if name in kinds and tool in ceiling["tools"] and interface in ceiling["interfaces"]
            and tool in policy["tools"] and interface in policy["interfaces"]
        )
        if not bindings:
            continue
        defaults = descriptor.get("boundary_defaults") or {}
        refs = (defaults.get("read_root_refs") or []) + (defaults.get("write_root_refs") or [])
        slots = host_slots.get(name) or {}
        require(isinstance(slots, dict), f"the root EE driver_host_slots of {name} is not an object")
        drivers.append({
            "name": name,
            "root_slots": {ref["path"] for ref in refs if isinstance(ref, dict) and isinstance(ref.get("path"), str) and ref["path"]},
            "doors": sorted({interface for _, interface in bindings}),
            "tools": [{"tool": tool, "interface": interface, "worker_environment_kind": name} for tool, interface in bindings],
            "host_slots": dict(slots),
        })
    return drivers


def offered_applications(layout, policy, ceiling, tools):
    offered, left_out = [], []
    for name in sorted(os.listdir(layout["interfaces"])):
        path = os.path.join(layout["interfaces"], name, "application.json")
        if not os.path.isfile(path):
            continue
        manifest = read_json(path, f"application manifest {name}")
        require(isinstance(manifest, dict) and manifest.get("id") == name, f"{path} does not declare application {name}")
        if manifest.get("category") not in APPLICATION_CATEGORIES:
            continue
        interfaces = manifest.get("interfaces")
        capabilities = (manifest.get("task_capabilities") or {}).get("tools") or []
        bindings = manifest.get("artifact_bindings") or []
        require(
            isinstance(interfaces, list) and interfaces and all(isinstance(value, str) for value in interfaces)
            and isinstance(capabilities, list) and all(isinstance(row, dict) for row in capabilities)
            and isinstance(bindings, list) and all(isinstance(row, dict) and isinstance(row.get("artifact_id"), str) for row in bindings),
            f"{path} is a malformed application manifest",
        )
        receipt = os.path.join(layout["home"], "client", "package-state", "receipts", f"{name}.json")
        if not os.path.isfile(receipt) or any(not os.path.isfile(os.path.join(layout["interfaces"], value, "interface.json")) for value in interfaces):
            continue
        if any(
            capability.get("tool") not in tools
            or tools[capability["tool"]][0] != capability.get("interface")
            or capability.get("worker_environment_kind") not in tools[capability["tool"]][1]
            for capability in capabilities
        ):
            left_out.append((name, "its task tools differ from their registry descriptors"))
            continue
        required = set(interfaces) | {row["interface"] for row in capabilities}
        if not (
            required <= policy["interfaces"]
            and {row["tool"] for row in capabilities} <= policy["tools"]
            and {row["worker_environment_kind"] for row in capabilities} <= policy["kinds"]
        ):
            left_out.append((name, "the root EE's formed policy does not allow it"))
            continue
        if not (
            set(interfaces) <= ceiling["interfaces"]
            and all(
                row["tool"] in ceiling["tools"] and row["interface"] in ceiling["interfaces"] and row["worker_environment_kind"] in ceiling["wek"]
                for row in capabilities
            )
        ):
            left_out.append((name, f"it is outside the {TASK_WORKER_ID} ceiling"))
            continue
        reason = None
        for interface in interfaces:
            reason = turn_starting(layout, interface)
            if reason:
                break
        if reason:
            left_out.append((name, reason))
            continue
        offered.append({
            "id": name,
            "interfaces": interfaces,
            "artifact_ids": [row["artifact_id"] for row in bindings],
            "tools": [{"tool": row["tool"], "interface": row["interface"], "worker_environment_kind": row["worker_environment_kind"]} for row in capabilities],
        })
    return offered, left_out


def turn_starting(layout, interface):
    private = (interface_json(layout["registry"], interface).get("interface_driver_config") or {}).get("private_config") or {}
    if "operation_catalog_dir" not in private:
        return f"its input from {interface} accepts every artifact and would start the task's turns"
    catalog_path = os.path.join(layout["registry"], "artifact", f"artifact-operation-catalog-{interface}", "artifact.json")
    if not os.path.isfile(catalog_path):
        return f"{interface} is not connected (it has no operation catalog yet)"
    catalog = read_json(catalog_path, f"operation catalog of {interface}")
    labels = catalog.get("task_input_labels") if isinstance(catalog, dict) else None
    if labels:
        return f"its task input {', '.join(labels)} would wake the task on every {interface} event"
    return None


def compact_route(row, *optional):
    return {key: value for key, value in row.items() if key not in optional or value}


def application_checkpoint(application_id, index):
    checkpoint = f"cp_app_{application_id}_input_{index + 1}"
    require(CHECKPOINT_PATTERN.match(checkpoint), f"application {application_id} cannot form a portable checkpoint id")
    return checkpoint


def working_folder_drivers(layout, home, profile, drivers, full_access):
    source_folders = home["octopus_os"].get("source_folders") or []
    require(isinstance(source_folders, list), "the root EE config.octopus_os.source_folders is not a list")
    host_path = os.path.join(layout["home"], "host.json")
    host = read_json(host_path, "host manifest") if os.path.isfile(host_path) else {}
    host_workspace = (host.get("root_refs") or {}).get(WORKSPACE_ROOT) if isinstance(host, dict) else None
    enabled, without_folder = [], []
    for driver in drivers:
        folder = (
            WORKSPACE_ROOT not in driver["root_slots"] or source_folders or host_workspace
            or WORKSPACE_ROOT in driver["host_slots"]
            or (full_access and HOST_FILESYSTEM_ROOT in driver["root_slots"])
            or all(WORKSPACE_ROOT in (profile["door_host_slots"].get(door) or {}) for door in driver["doors"])
        )
        (enabled if folder else without_folder).append(driver)
    return enabled, [driver["name"] for driver in without_folder], source_folders, host_workspace


def task_capabilities(layout, home, profile, drivers, applications, notice_label, full_access):
    drivers, without_folder, source_folders, host_workspace = working_folder_drivers(layout, home, profile, drivers, full_access)
    kinds = {driver["name"] for driver in drivers}
    left_out = [
        (f"Worker Environment {kind}", "it needs a working folder and the root EE has none (choose one, or drop --project-access)")
        for kind in without_folder
    ]
    for application in [row for row in applications if not {tool["worker_environment_kind"] for tool in row["tools"]} <= kinds]:
        left_out.append((application["id"], "it needs a Worker Environment this task cannot enable"))
        applications = [row for row in applications if row is not application]
    external_inputs = [compact_route(dict(row), "artifact_names", "required_verified_artifacts") for row in profile.get("external_inputs", [])]
    for row in external_inputs:
        reason = turn_starting(layout, row.get("interface_id"))
        require(
            reason is None,
            f"the root task profile routes {row.get('checkpoint_id')} from {row.get('interface_id')}: {reason}; "
            f"the woken task may start turns only from {TASK_CHECKPOINT_ID}",
        )
    external_relays = [compact_route(dict(row), "required_verified_artifacts") for row in profile.get("external_relays", [])]
    model_outputs = [dict(row) for row in profile.get("model_outputs", [])]
    contexts = [{"id": row.get("id")} for row in profile.get("context_artifact_bindings", [])]
    selected = {}
    for row in profile.get("selected_tools", []):
        if row.get("worker_environment_kind") in kinds:
            selected[row.get("tool")] = {"tool": row.get("tool"), "interface": row.get("interface"), "worker_environment_kind": row.get("worker_environment_kind")}

    def claim(capability, owner):
        existing = selected.get(capability["tool"])
        require(
            existing is None or (existing["interface"], existing["worker_environment_kind"]) == (capability["interface"], capability["worker_environment_kind"]),
            f"{owner} tool {capability['tool']} has more than one exact owner",
        )
        selected[capability["tool"]] = dict(capability)

    inputs = {row.get("interface_id") for row in external_inputs}
    checkpoints = {row.get("checkpoint_id") for row in external_inputs + external_relays + model_outputs}
    artifacts = {row["id"] for row in contexts}
    for driver in drivers:
        for capability in driver["tools"]:
            claim(capability, f"Worker Environment {driver['name']}")
    for application in applications:
        for index, interface in enumerate(application["interfaces"]):
            if interface in inputs:
                continue
            checkpoint = application_checkpoint(application["id"], index)
            require(checkpoint not in checkpoints, f"application checkpoint {checkpoint} is already authored")
            checkpoints.add(checkpoint)
            inputs.add(interface)
            external_inputs.append({"checkpoint_id": checkpoint, "interface_id": interface})
        for artifact_id in application["artifact_ids"]:
            if artifact_id not in artifacts:
                artifacts.add(artifact_id)
                contexts.append({"id": artifact_id})
        for capability in application["tools"]:
            claim(capability, f"application {application['id']}")
    require(
        TASK_CHECKPOINT_ID not in checkpoints and SENSOR_ID not in inputs,
        f"the task authoring profile already routes {TASK_CHECKPOINT_ID} or {SENSOR_ID}",
    )
    external_inputs.append({"checkpoint_id": TASK_CHECKPOINT_ID, "interface_id": SENSOR_ID, "artifact_names": [notice_label]})

    doors = {door for driver in drivers for door in driver["doors"]}
    slots = {door: dict(row) for door, row in profile["door_host_slots"].items() if door in doors}
    owners = {}
    for driver in drivers:
        for door in driver["doors"]:
            row = slots.get(door, {})
            for slot, root in driver["host_slots"].items():
                owner = owners.get((door, slot))
                require(owner is None or row.get(slot) == root, f"Worker Environments {owner} and {driver['name']} configure different roots for {slot} on {door}")
                row[slot] = root
                owners[(door, slot)] = driver["name"]
            if row:
                slots[door] = row
    for driver in drivers:
        if WORKSPACE_ROOT in driver["root_slots"] and source_folders:
            for door in driver["doors"]:
                slots[door] = {**slots.get(door, {}), WORKSPACE_ROOT: source_folders[0]}
    host_root = os.path.splitdrive(layout["home"])[0] + os.sep
    full = [driver for driver in drivers if HOST_FILESYSTEM_ROOT in driver["root_slots"] and driver["doors"]] if full_access else []
    require(full or not full_access, "full access requires an offered Worker Environment that declares the host filesystem root (or pass --project-access)")
    for driver in full:
        for door in driver["doors"]:
            row = slots.get(door, {})
            require(
                row.get(HOST_FILESYSTEM_ROOT, host_root) == host_root,
                f"Worker Environment {driver['name']} configures {HOST_FILESYSTEM_ROOT} as {row.get(HOST_FILESYSTEM_ROOT)}, not the host root {host_root}",
            )
            row[HOST_FILESYSTEM_ROOT] = host_root
            if WORKSPACE_ROOT in driver["root_slots"] and not row.get(WORKSPACE_ROOT):
                row[WORKSPACE_ROOT] = host_root
            slots[door] = row
    require(
        not any(WORKSPACE_ROOT in driver["root_slots"] and not slots.get(door, {}).get(WORKSPACE_ROOT) and not host_workspace for driver in drivers for door in driver["doors"]),
        "this task has no working folder; choose a project folder for the root EE first",
    )
    return [row["id"] for row in applications], left_out, {
        "door_host_slots": slots,
        "external_inputs": external_inputs,
        "external_relays": external_relays,
        "model_outputs": model_outputs,
        "context_artifact_bindings": contexts,
        "payload_artifact_bindings": [dict(row) for row in profile.get("payload_artifact_bindings", [])],
        "selected_worker_environment_kinds": sorted(kinds),
        "selected_tools": sorted(selected.values(), key=lambda row: row["tool"]),
    }


def check_ee_chain(layout, ee_id):
    missing = set()
    seen = set()
    current = ee_id
    while current is not None:
        require(current not in seen, f"the EE chain of {ee_id} loops at {current}")
        seen.add(current)
        instance = read_json(os.path.join(layout["ee_instances"], f"{current}.json"), f"EE instance {current}")
        require(isinstance(instance, dict) and instance.get("ee_id") == current, f"EE instance {current} does not name itself")
        template_name = instance.get("template_name")
        require(isinstance(template_name, str) and template_name, f"EE instance {current} names no template")
        template = read_json(
            os.path.join(layout["registry"], "ee_template", template_name, "ee_template.json"),
            f"EE template {template_name}",
        )
        narrowing = instance.get("policy_narrowing") or {}
        require(isinstance(template, dict) and isinstance(narrowing, dict), f"EE {current} policy is malformed")
        for layer, name in ((template, f"template {template_name}"), (narrowing, f"instance {current}")):
            allowed = layer.get("allowed_interfaces")
            if allowed is None:
                continue
            require(isinstance(allowed, list), f"EE {name} allowed_interfaces is not a list")
            missing.update(f"{interface_id} (by {name})" for interface_id in CHAIN_INTERFACES if interface_id not in allowed)
        current = instance.get("parent_ee_id")
    require(
        not missing,
        f"the EE chain of {ee_id} does not admit what the relay needs: {', '.join(sorted(missing))}; "
        "allow these interfaces for the root EE first (the Home's Applications settings narrow it)",
    )


def email_labels(layout):
    folder = os.path.join(layout["interfaces"], EMAIL_ID)
    descriptor = interface_json(layout["registry"], EMAIL_ID)
    require(os.path.isfile(os.path.join(folder, "life.json")), f"{EMAIL_ID} is not installed with a life: {folder}")
    config_path = os.path.join(folder, "config.json")
    config = read_json(config_path, "Email config")
    authz = config.get("authz") if isinstance(config, dict) else None
    require(
        isinstance(authz, dict)
        and isinstance(authz.get("allow_all_senders"), bool)
        and isinstance(authz.get("ignore_automated"), bool),
        f"{config_path} has no boolean authz.allow_all_senders / authz.ignore_automated",
    )
    event_label = (config.get("intake") or {}).get("event_label")
    require(event_label == EVENT_LABEL, f"{EMAIL_ID} hands {event_label!r}, but the sensor reads {EVENT_LABEL}")
    private = (descriptor.get("interface_driver_config") or {}).get("private_config") or {}
    labels = {event_label}
    catalog_label = private.get("operation_catalog_label")
    if isinstance(catalog_label, str) and catalog_label:
        labels.add(catalog_label)
    return config_path, sorted(labels)


def relay_record(ee_id, inbound_labels):
    return {
        "id": RELAY_RUN_ID,
        "ee": ee_id,
        "worker": RELAY_WORKER_ID,
        "trace": f"trace:{RELAY_RUN_ID}",
        "status": "running",
        "interface_state": {},
        "checkpoints": [
            {
                "id": RELAY_CHECKPOINT_ID,
                "run": RELAY_RUN_ID,
                "trace": f"trace:{RELAY_RUN_ID}:{RELAY_CHECKPOINT_ID}",
                "epoch": 0,
                "interface_binding": {
                    "interface": EMAIL_ID,
                    "config": {"artifact_policy": {"mode": "allow", "names": inbound_labels, "namespaces": []}},
                },
                "outbound_interface_bindings": [
                    {
                        "interface": SENSOR_ID,
                        "config": {"artifact_policy": {"mode": "allow", "names": [EVENT_LABEL], "namespaces": []}},
                    }
                ],
                "artifact_bindings": [],
                "required_verified_artifacts": [],
            }
        ],
        "active_checkpoints": [RELAY_CHECKPOINT_ID],
    }


def receiving_tasks(records):
    found = []
    for run_id, record in records.items():
        for checkpoint in record.get("checkpoints") or []:
            binding = checkpoint.get("interface_binding") if isinstance(checkpoint, dict) else None
            if isinstance(binding, dict) and binding.get("interface") == SENSOR_ID:
                found.append(run_id)
                break
    return found


def kit_sources():
    for interface_id, files in KIT_FILES.items():
        for name in files:
            path = os.path.join(KIT_INTERFACES, interface_id, name)
            require(os.path.isfile(path), f"the kit is incomplete: {path} is missing")
    watch = read_json(os.path.join(KIT_INTERFACES, SENSOR_ID, "watch.json"), "watch.json")
    label = watch.get("notice_label") if isinstance(watch, dict) else None
    require(isinstance(label, str) and label and label.strip() == label, "watch.json names no notice_label")
    instructions = watch.get("instructions")
    require(isinstance(instructions, str), "watch.json instructions is not a string")
    return label, instructions


def armed_document():
    armed_at = datetime.datetime.now(datetime.timezone.utc).replace(microsecond=0).isoformat()
    return armed_at, pretty({"armed_at": armed_at}).encode("utf-8")


def current_armed(target):
    path = os.path.join(target, ARMED_FILE)
    if not os.path.isfile(path):
        return None
    with open(path, "rb") as handle:
        content = handle.read()
    armed = read_json(path, "installed armed.json")
    require(isinstance(armed, dict) and isinstance(armed.get("armed_at"), str), f"{path} is malformed")
    return armed["armed_at"], content


def sensor_state(layout):
    folder = os.path.join(layout["private_state"], SENSOR_ID)
    fired_path = os.path.join(folder, "fired.json")
    fired = read_json(fired_path, "sensor fired state") if os.path.lexists(fired_path) else None
    require(fired is None or isinstance(fired, dict), f"{fired_path} is malformed")
    return folder, fired_path, fired


def describe_fired(fired):
    if fired is None:
        return "armed, nothing fired yet"
    status = fired.get("status")
    if status == "admitted":
        return f"fired: the notice was admitted at ledger seq {fired.get('admitted_seq')}"
    if status == "refused":
        return f"spent: the notice was refused ({(fired.get('refusal') or {}).get('reason')})"
    return f"{status}: a notice from ledger seq {fired.get('source_movement_seq')} is waiting for admission"


def staging_area(layout):
    for name in sorted(os.listdir(layout["registry"])):
        if name.startswith(STAGING_PREFIX):
            leftover = os.path.join(layout["registry"], name)
            shutil.rmtree(leftover)
            say(f"removed {leftover}, left by an interrupted earlier run")
    area = os.path.join(layout["registry"], f"{STAGING_PREFIX}{os.getpid()}-{secrets.token_hex(4)}")
    os.mkdir(area, 0o700)
    return area


def move_back(source, destination):
    require(not os.path.lexists(destination), f"{destination} reappeared; {source} keeps the earlier copy")
    os.rename(source, destination)


def install_folder(layout, area, interface_id, armed, undo):
    target = os.path.join(layout["interfaces"], interface_id)
    staging = os.path.join(area, f"new-{interface_id}")
    os.mkdir(staging, 0o755)
    for name in KIT_FILES[interface_id]:
        shutil.copyfile(os.path.join(KIT_INTERFACES, interface_id, name), os.path.join(staging, name))
        os.chmod(os.path.join(staging, name), 0o644)
    if armed is not None:
        with open(os.path.join(staging, ARMED_FILE), "wb") as handle:
            handle.write(armed)
        os.chmod(os.path.join(staging, ARMED_FILE), 0o644)
    sync_directory(staging)
    if os.path.lexists(target):
        retired = os.path.join(area, f"old-{interface_id}")
        os.rename(target, retired)
        undo.add(f"retired {target}", lambda: move_back(retired, target))
    os.rename(staging, target)
    undo.add(f"installed {target}", lambda: os.rename(target, os.path.join(area, f"undone-{interface_id}")))
    sync_directory(layout["interfaces"])
    say(f"wrote folder {target}")


def set_flag(text, key, value):
    pattern = re.compile(r'("' + re.escape(key) + r'"\s*:\s*)(true|false)\b')
    require(len(pattern.findall(text)) == 1, f"the Email config does not hold exactly one boolean {key}")
    return pattern.sub(lambda match: match.group(1) + json.dumps(value), text)


def open_email(config_path, undo):
    with open(config_path, "rb") as handle:
        current = handle.read()
    config = json.loads(current)
    if config["authz"]["allow_all_senders"] is True and config["authz"]["ignore_automated"] is False:
        say(f"kept {config_path} (already admits every sender, automated mail included)")
    else:
        text = set_flag(set_flag(current.decode("utf-8"), "allow_all_senders", True), "ignore_automated", False)
        intended = json.loads(current)
        intended["authz"]["allow_all_senders"] = True
        intended["authz"]["ignore_automated"] = False
        require(json.loads(text) == intended, f"editing the two authz flags of {config_path} would change more than them")
        written = text.encode("utf-8")
        replace_file(config_path, written, octopus_content_hash(current))
        undo.add(f"opened {config_path}", lambda: restore_replaced(config_path, current, written))
        say(f"wrote {config_path}")
    say("NOTE: this Home's Email now admits every sender, automated and bulk mail included.")


def plan_request(layout, home, provider, model, effort, service_tier, chat_id, capabilities):
    return {
        "registry_root": layout["registry"],
        "ee_instances_root": layout["ee_instances"],
        "ee_policy_root": layout["ee_instances"],
        "run_records_root": layout["runs"],
        "write_hash_algorithm": "blake3",
        "interface_private_state_root": layout["private_state"],
        "chat_id": chat_id,
        "provider_interface_id": provider,
        "model": model,
        "reasoning_effort": effort or None,
        "service_tier": service_tier or None,
        "ee_id": home["ee_id"],
        "worker_interface_id": TASK_WORKER_ID,
        "client_surface_role": "task",
        "cycle": chat_id,
        **capabilities,
        "publications": [],
        "existing_run_record": None,
        "retain_current_capabilities": False,
        "conversation": {
            "ledger_offset": 0,
            "artifact_bindings": [],
            "pinned_artifact_bindings": [],
            "pinned_context_items": [],
            "continued_from": None,
        },
    }


def requested_profile(request):
    profile = {"ee_id": request["ee_id"], "worker_choice_interface": request["worker_interface_id"], "door_host_slots": request["door_host_slots"]}
    profile.update({field: request[field] for field in PROFILE_FIELDS if request[field]})
    return profile


def invoke_planner(planner, request):
    completed = subprocess.run([planner], input=json.dumps(request).encode("utf-8"), capture_output=True, timeout=120, check=False)
    if completed.returncode != 0:
        raise Refusal(f"the task planner failed ({completed.returncode}): {completed.stderr.decode('utf-8', 'replace').strip()}")
    try:
        plan = json.loads(completed.stdout)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise Refusal(f"the task planner returned malformed JSON: {error}") from None
    return plan


def validate_plan(plan, layout, home, request, notice_label, protected):
    chat_id = request["chat_id"]
    require(isinstance(plan, dict), "the task plan is not an object")
    missing = PLAN_REQUIRED - set(plan)
    unknown = set(plan) - PLAN_ALLOWED
    require(not missing and not unknown, f"the task plan keys differ: missing {sorted(missing)}, unknown {sorted(unknown)}")
    require(plan["expected_before_hash_algorithm"] == "blake3", "the task plan did not form BLAKE3 write guards")
    expected = requested_profile(request)
    expansion = plan.get("publication_expansion")
    if expansion is None:
        require(plan.get("authoring_profile") == expected, "the planned task authoring profile differs from the requested grant")
    else:
        require(
            isinstance(expansion, dict) and expansion.get("requested") == expected and expansion.get("expanded") == plan.get("authoring_profile"),
            "the planned publication expansion does not preserve the requested grant",
        )
    require(plan["run_id"] == f"run_operator_chat_{chat_id}", "the task plan names an unexpected run id")
    require(plan["chat_interface_id"] == f"world_operator_chat_{chat_id}", "the task plan names an unexpected client interface")
    require(plan["client_surface_role"] == "task", "the task plan changed the client surface role")
    require(plan["removals"] == [], "a new task plan must not remove files")
    require(isinstance(plan["interface_writes"], list) and plan["interface_writes"], "the task plan has no interface writes")
    require(isinstance(plan["run_writes"], list), "the task plan run_writes is not a list")
    writes = plan["interface_writes"] + plan["run_writes"]
    run_path = os.path.join(layout["runs"], f"{plan['run_id']}.json")
    seen = set()
    for index, write in enumerate(writes):
        require(isinstance(write, dict) and set(write) == {"path", "content", "tag", "expected_before_hash"}, f"task plan write #{index} is malformed")
        path = write["path"]
        require(isinstance(path, str) and os.path.isabs(path) and path not in seen and path != run_path, f"task plan write #{index} has an invalid path")
        require(inside(path, layout["home"]), f"task plan write #{index} leaves the Home: {path}")
        require(not any(path == root or inside(path, root) for root in protected), f"task plan write #{index} touches what this kit owns: {path}")
        require(isinstance(write["content"], str), f"task plan write #{index} has no text content")
        expected = write["expected_before_hash"]
        require(expected is None or (isinstance(expected, str) and len(expected) == 64), f"task plan write #{index} has an invalid expected_before_hash")
        seen.add(path)
    try:
        run = json.loads(plan["run_record_content"])
    except (TypeError, json.JSONDecodeError) as error:
        raise Refusal(f"the planned task run record is malformed: {error}") from None
    require(
        run.get("id") == plan["run_id"] and run.get("ee") == home["ee_id"] and run.get("worker") == plan["worker_interface_id"],
        "the planned task run record identity is inconsistent",
    )
    checkpoints = [row for row in run.get("checkpoints", []) if isinstance(row, dict) and row.get("id") == TASK_CHECKPOINT_ID]
    require(len(checkpoints) == 1, f"the planned task has no single {TASK_CHECKPOINT_ID} checkpoint")
    checkpoint = checkpoints[0]
    binding = checkpoint.get("interface_binding") or {}
    policy = (binding.get("config") or {}).get("artifact_policy") or {}
    require(
        binding.get("interface") == SENSOR_ID and policy.get("mode") == "allow" and notice_label in (policy.get("names") or []),
        f"{TASK_CHECKPOINT_ID} does not admit {notice_label} from {SENSOR_ID}",
    )
    outbound = [row.get("interface") for row in checkpoint.get("outbound_interface_bindings", [])]
    require(outbound == [plan["model_interface_id"]], f"{TASK_CHECKPOINT_ID} does not lead only to the task's model interface")
    require(TASK_CHECKPOINT_ID in run.get("active_checkpoints", []), f"{TASK_CHECKPOINT_ID} is not active")
    return run_path


def titled(plan, layout, title):
    sidecar_path = os.path.join(layout["interfaces"], plan["chat_interface_id"], "octopus_os.json")
    rows = [write for write in plan["interface_writes"] if write["path"] == sidecar_path]
    require(len(rows) == 1, f"the task plan writes no client sidecar at {sidecar_path}")
    sidecar = json.loads(rows[0]["content"])
    require(
        sidecar.get("component") == "client"
        and sidecar.get("surface_role") == "task"
        and sidecar.get("owner_interface") == plan["chat_interface_id"]
        and sidecar.get("interface") == plan["chat_interface_id"]
        and "presentation" not in sidecar,
        "the planned client sidecar is not a fresh task client sidecar",
    )
    sidecar["presentation"] = {"name": title, "projects": []}
    rows[0]["content"] = pretty(sidecar)


def precheck_writes(writes):
    for write in writes:
        if write["expected_before_hash"] is None:
            require(not os.path.lexists(write["path"]), f"{write['path']} exists although the plan expected it absent")
        else:
            with open(write["path"], "rb") as handle:
                current = handle.read()
            require(octopus_content_hash(current) == write["expected_before_hash"], f"{write['path']} changed after it was planned")


def apply_writes(writes, undo):
    for write in writes:
        path = write["path"]
        content = write["content"].encode("utf-8")
        if write["expected_before_hash"] is None:
            create_file(path, content, undo)
        else:
            with open(path, "rb") as handle:
                previous = handle.read()
            replace_file(path, content, write["expected_before_hash"])
            undo.add(f"replaced {path}", lambda path=path, previous=previous, content=content: restore_replaced(path, previous, content))
        say(f"wrote {path}")


def bundled_binaries(arguments):
    app = os.path.abspath(arguments.app) if arguments.app else None
    binaries = []
    for flag, given, name in (("--planner", arguments.planner, PLANNER_NAME), ("--runtime", arguments.runtime, RUNTIME_NAME)):
        require(given or app, f"name the installed Octopus.app with --app, or pass {flag}")
        path = os.path.abspath(given) if given else os.path.join(app, APP_BIN, name)
        require(os.path.isfile(path) and os.access(path, os.X_OK), f"{flag} is not an executable file: {path}")
        binaries.append(path)
    return binaries


def author(arguments):
    home_path = os.path.realpath(arguments.home)
    layout = home_layout(home_path)
    title = arguments.title.strip()
    require(title and title == arguments.title and len(title) <= 80, "--title must be trimmed, non-empty and at most 80 characters")
    planner, runtime = bundled_binaries(arguments)
    notice_label, instructions = kit_sources()
    records = run_records(layout)
    home = home_run(layout, records)
    check_ee_chain(layout, home["ee_id"])
    config_path, inbound_labels = email_labels(layout)
    provider = arguments.provider or home["provider"]
    if provider == home["provider"]:
        model = arguments.model or home["model"]
        effort = arguments.effort or home["effort"]
        service_tier = home["service_tier"]
    else:
        require(arguments.model, f"--provider {provider} differs from the Home's {home['provider']}; name its --model too")
        model, effort, service_tier = arguments.model, arguments.effort, None
    require(isinstance(model, str) and model, "no model: the Home's provider interface records none; pass --model")
    interface_json(layout["registry"], provider)
    relay = relay_record(home["ee_id"], inbound_labels)
    relay_path = os.path.join(layout["runs"], f"{RELAY_RUN_ID}.json")
    relay_content = pretty(relay).encode("utf-8")
    existing_tasks = receiving_tasks(records)
    if not existing_tasks:
        profile = task_profile(home)
        policy = formed_policy(runtime, layout, home["ee_id"], provider, model)
        ceiling = worker_ceiling(layout)
        tools = registry_tools(layout)
        drivers = offered_drivers(layout, policy, ceiling, tools, home["octopus_os"])
        applications, left_out = offered_applications(layout, policy, ceiling, tools)
        connected, unavailable, capabilities = task_capabilities(layout, home, profile, drivers, applications, notice_label, not arguments.project_access)
    sensor_folder = os.path.join(layout["interfaces"], SENSOR_ID)
    replace = arguments.replace or arguments.rearm
    if not replace:
        for interface_id in KIT_FILES:
            target = os.path.join(layout["interfaces"], interface_id)
            require(not os.path.lexists(target), f"{target} already exists; pass --replace to replace the kit's folders")
        require(not existing_tasks, f"a task already receives {SENSOR_ID}: {', '.join(existing_tasks)}")
        require(not os.path.lexists(relay_path), f"{relay_path} already exists")
    elif os.path.lexists(relay_path):
        with open(relay_path, "rb") as handle:
            require(json.loads(handle.read()) == relay, f"{relay_path} exists with other content; it is left for the owner")
    state_folder, fired_path, fired = sensor_state(layout)
    installed_armed = current_armed(sensor_folder) if os.path.isdir(sensor_folder) else None
    if fired is not None and not arguments.rearm:
        require(
            installed_armed is not None and bool(existing_tasks),
            f"{fired_path} survives an earlier install ({describe_fired(fired)}); pass --rearm to arm this Home again",
        )
    if arguments.rearm or installed_armed is None or not existing_tasks:
        armed_at, armed = armed_document()
    else:
        armed_at, armed = installed_armed
    planned = existing_tasks[0] if existing_tasks else None
    email_state = os.path.join(layout["private_state"], EMAIL_ID, "mailbox-state.json")
    protected = [os.path.join(layout["interfaces"], name) for name in (EMAIL_ID, *KIT_FILES)] + [relay_path, state_folder]

    undo = Undo()
    area = staging_area(layout)
    try:
        if arguments.rearm:
            for name in ("fired.json", "payloads"):
                source = os.path.join(state_folder, name)
                if os.path.lexists(source):
                    parked = os.path.join(area, f"state-{name}")
                    os.rename(source, parked)
                    undo.add(f"cleared {source}", lambda parked=parked, source=source: move_back(parked, source))
                    say(f"cleared {source} ({describe_fired(fired) if name == 'fired.json' else 'prepared notices'})")
        install_folder(layout, area, SENSOR_ID, armed, undo)
        install_folder(layout, area, RELAY_WORKER_ID, None, undo)

        plan = None
        if planned is None:
            chat_id = f"legacy_watch_{secrets.token_hex(8)}"
            request = plan_request(layout, home, provider, model, effort, service_tier, chat_id, capabilities)
            plan = invoke_planner(planner, request)
            run_path = validate_plan(plan, layout, home, request, notice_label, protected)
            titled(plan, layout, title)
            writes = plan["interface_writes"] + plan["run_writes"]
            precheck_writes(writes)
            require(not os.path.lexists(run_path), f"{run_path} already exists")

        if arguments.any_sender:
            open_email(config_path, undo)

        if plan is None:
            task_run_id = planned
            say(f"kept the receiving task {task_run_id}")
        else:
            apply_writes(writes, undo)
            create_file(run_path, plan["run_record_content"].encode("utf-8"), undo)
            say(f"wrote {run_path}")
            task_run_id = plan["run_id"]

        if os.path.lexists(relay_path):
            say(f"kept {relay_path}")
        else:
            create_file(relay_path, relay_content, undo)
            say(f"wrote {relay_path}")
    except Exception as error:
        failures = undo.run()
        shutil.rmtree(area, ignore_errors=True)
        reason = str(error) if isinstance(error, Refusal) else f"{type(error).__name__}: {error}"
        if failures:
            raise Refusal(f"{reason}; undoing the earlier steps also failed: {'; '.join(failures)}") from None
        raise Refusal(f"{reason}; every earlier step of this run was undone") from None
    shutil.rmtree(area)

    say("")
    say(f"The trigger is authored in {home_path}.")
    say(f"  armed at: {armed_at} (letters Email received earlier never fire)")
    say(f"  receiving task: {task_run_id} in {home['ee_id']}, titled {title!r} (the Sidebar lists it with no project filter)")
    if plan is not None:
        if not arguments.project_access:
            say(f"  task access: full machine access (host filesystem root {os.path.splitdrive(home_path)[0] + os.sep})")
        else:
            say(f"  task access: the root task profile's own door slots {json.dumps(capabilities['door_host_slots'], ensure_ascii=False)}")
        say("  task grant:")
        say(f"    Worker Environments: {', '.join(capabilities['selected_worker_environment_kinds'])}")
        say(f"    tools: {len(capabilities['selected_tools'])}")
        say(f"    applications: {', '.join(connected) or 'none'}")
        for application_id, reason in unavailable + left_out:
            say(f"    left out {application_id}: {reason}")
        unoffered = [kind for kind in profile.get("selected_worker_environment_kinds", []) if kind not in capabilities["selected_worker_environment_kinds"]]
        if unoffered:
            say(f"NOTE: the task profile selects {', '.join(unoffered)}, which this Home does not offer to {TASK_WORKER_ID}.")
    say(f"  relay: {RELAY_RUN_ID} admits {', '.join(inbound_labels)} from {EMAIL_ID} and passes only {EVENT_LABEL} to {SENSOR_ID}")
    say(f"  senders: {email_senders(read_json(config_path, 'Email config')['authz'])}")
    say(f"  sensor state: {fired_path} ({describe_fired(None if arguments.rearm else fired)}; prepared, then admitted or refused)")
    say(f"  sensor cursor and log: {state_folder}/ledger-cursor.json, {state_folder}/interface-life.stderr.*.log")
    say(f"  terms and instructions: {os.path.join(sensor_folder, 'watch.json')}")
    if not instructions:
        say("NOTE: watch.json instructions is empty; the woken task receives the letter alone until you write the prompt there.")
    if os.path.lexists(email_state) and not any(
        run_id != RELAY_RUN_ID and binds_email(record) for run_id, record in records.items()
    ):
        say(f"NOTE: {EMAIL_ID} was used before and is idle; it now catches up from {email_state} and hands that mail into the ledger, where it never fires.")


def email_senders(authz):
    if authz["allow_all_senders"]:
        who = "every sender reaches the trigger"
    elif authz.get("include_account_address"):
        who = "only the mailbox's own address and the Allowed senders set in Email reach the trigger"
    else:
        who = "only the Allowed senders set in Email reach the trigger"
    automated = "automated mail included" if not authz["ignore_automated"] else "automated mail is dropped"
    return f"{who}, {automated}"


def binds_email(record):
    active = set(record.get("active_checkpoints") or [])
    for checkpoint in record.get("checkpoints") or []:
        if not isinstance(checkpoint, dict) or checkpoint.get("id") not in active:
            continue
        binding = checkpoint.get("interface_binding")
        if isinstance(binding, dict) and binding.get("interface") == EMAIL_ID:
            return True
    return False


def main():
    parser = argparse.ArgumentParser(description="Author the legacy watch trigger into an Octopus Home.")
    parser.add_argument("--home", required=True)
    parser.add_argument("--provider")
    parser.add_argument("--model")
    parser.add_argument("--effort")
    parser.add_argument("--title", default=DEFAULT_TITLE)
    parser.add_argument("--replace", action="store_true", help="replace the kit's folders, keeping the receiving task with its arming and fired state; a new receiving task is armed from now")
    parser.add_argument("--rearm", action="store_true", help="replace the kit's folders, arm from now and clear the fired state")
    parser.add_argument("--app", help="the installed Octopus.app whose bundled planner and runtime are used, e.g. /Applications/Octopus.app")
    parser.add_argument("--planner", help="the task topology planner; overrides the one in --app")
    parser.add_argument("--runtime", help="the octopus binary whose project-ee-policies forms the root EE policy; overrides the one in --app")
    parser.add_argument("--project-access", action="store_true", help="give the woken task the root task profile's own door slots instead of the whole machine")
    parser.add_argument("--any-sender", action="store_true", help="open this Home's Email to every sender, automated mail included, so a matching letter from anyone fires the trigger")
    arguments = parser.parse_args()
    try:
        author(arguments)
    except Refusal as refusal:
        sys.stderr.write(f"legacy watch was not authored further: {refusal}\n")
        raise SystemExit(2)


if __name__ == "__main__":
    main()
