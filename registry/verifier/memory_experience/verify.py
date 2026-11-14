#!/usr/bin/env python3
import json
import re
import sys


def exact(value):
    return isinstance(value, str) and bool(value) and value.strip() == value


def reference(value):
    return isinstance(value, dict) and set(value) == {"id", "content"} and exact(value["id"]) and isinstance(value["content"], list) and len(value["content"]) == 32 and all(type(byte) is int and 0 <= byte <= 255 for byte in value["content"])


def verify(request):
    params = request["params"]
    limit = params["max_bytes"]
    assert isinstance(limit, int) and 0 < limit <= 268435456, "invalid declared byte bound"
    with open(request["payload_path"], "rb") as handle:
        data = handle.read(limit + 1)
    assert len(data) <= limit, "experience exceeds its declared bound"
    value = json.loads(data)
    assert isinstance(value, dict), "experience must be an object"
    if params["stage"] == "terminal":
        snapshot = value["session_context"]
        assert request["artifact_label"] == "interface.artifact.output", "terminal label mismatch"
        assert snapshot["schema_version"] == 3 and snapshot["provider_interface"] == params["source_provider_interface"], "terminal snapshot identity mismatch"
        assert isinstance(snapshot["artifacts"], list) and isinstance(value["text"], str), "terminal snapshot fields malformed"
        return
    assert params["stage"] == "projection", "unknown experience stage"
    assert request["artifact_label"] == value["label"] == "memory.experience.input" and value["schema_version"] == 1, "projected experience identity mismatch"
    assert "session_context" not in value and exact(value["text"]), "foreign continuity or absent ordinary input text"
    source = value["source"]
    for field, key in [("run", "source_run"), ("provider_interface", "source_provider_interface"), ("client_interface", "source_client_interface")]:
        assert source[field] == params[key], "projected source identity mismatch: " + field
    assert exact(source["ee"]), "projected source has no observed EE"
    assert exact(source["worker_interface"]), "projected source has no observed worker"
    assert reference(source["terminal_artifact"]), "terminal reference malformed"
    assert isinstance(source["terminal_source_movement_seq"], int) and isinstance(source["projection_source_movement_seq"], int), "original terminal movement proof absent"
    selected = value["selected_input"]
    assert reference(selected["source_artifact"]) and isinstance(selected["source_movement_seq"], int) and isinstance(selected["fields"], dict), "selected input evidence malformed"
    assert selected["provenance"]["source_artifact"] == selected["source_artifact"] and selected["provenance"]["source_movement_seq"] == selected["source_movement_seq"], "selected input proof mismatch"
    assert isinstance(value["evidence"], list) and isinstance(value["omission"], dict), "evidence omissions absent"
    omission = value["omission"]
    fields_trimmed, evidence_trimmed = omission["selected_fields_trimmed"], omission["selected_evidence_trimmed"]
    assert type(fields_trimmed) is bool and type(evidence_trimmed) is bool, "trimming flags malformed"
    assert all(isinstance(item, dict) for item in value["evidence"]), "evidence entry malformed"
    field_groups = [(item.get("fields"), item.get("trimmed_fields", [])) for item in value["evidence"]]
    field_groups.extend([
        (selected["fields"], selected.get("trimmed_fields", [])),
        (selected.get("original_fields", {}), selected.get("original_trimmed_fields", [])),
        (value.get("terminal_fields", {}), value.get("terminal_trimmed_fields", [])),
    ])
    trimmed_fields = []
    for fields, entries in field_groups:
        assert isinstance(fields, dict) and isinstance(entries, list), "trimmed field container malformed"
        assert all(isinstance(entry, dict) and exact(entry.get("pointer")) and entry["pointer"] in fields and type(entry.get("bytes")) is int and entry["bytes"] > 0 for entry in entries), "trimmed field record malformed"
        assert len({entry["pointer"] for entry in entries}) == len(entries), "duplicate trimmed field record"
        trimmed_fields.extend(entries)
    assert fields_trimmed == bool(trimmed_fields), "field trimming is not recorded"
    trimmed_entries = omission.get("trimmed_evidence_entries", 0)
    assert type(trimmed_entries) is int and trimmed_entries >= 0 and evidence_trimmed == (trimmed_entries > 0), "evidence trimming is not recorded"


if __name__ == "__main__":
    try:
        verify(json.load(sys.stdin))
        result = {"passed": True, "details": {"contract": "memory_experience"}}
    except (AssertionError, KeyError, TypeError, ValueError, OSError) as error:
        result = {"passed": False, "details": {"contract": "memory_experience", "error": str(error)}}
    print(json.dumps(result))
