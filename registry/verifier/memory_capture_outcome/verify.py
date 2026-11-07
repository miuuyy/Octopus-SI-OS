#!/usr/bin/env python3
import json
import os
import re
import sys


def reference_key(value):
    assert isinstance(value, dict) and set(value) == {"id", "content"}, "invalid receipt reference"
    assert isinstance(value["id"], str) and value["id"] and value["id"].strip() == value["id"], "invalid receipt identity"
    assert isinstance(value["content"], list) and len(value["content"]) == 32 and all(type(byte) is int and 0 <= byte <= 255 for byte in value["content"]), "invalid receipt hash"
    return value["id"], tuple(value["content"])


def verify(request):
    params = request["params"]
    limit = params["max_bytes"]
    assert isinstance(limit, int) and 0 < limit <= 268435456, "invalid outcome byte bound"
    with open(request["payload_path"], "rb") as handle:
        data = handle.read(limit + 1)
    assert len(data) <= limit, "outcome exceeds its declared bound"
    payload = json.loads(data)
    assert request["artifact_label"] == payload["label"] == "memory.capture.outcome" and payload["schema_version"] == 1, "outcome label mismatch"
    source = payload["source"]
    assert source["run"] == params["source_run"] and source["provider_interface"] == params["provider_interface"], "outcome evidence source mismatch"
    observed_ee = source["ee"]
    assert isinstance(observed_ee, str) and observed_ee and observed_ee.strip() == observed_ee, "outcome evidence has no observed EE"
    reference_key(source["terminal_artifact"])
    assert isinstance(source["terminal_source_movement_seq"], int), "original observer terminal movement absent"
    experience = payload["experience"]
    reference_key(experience["source_artifact"])
    assert isinstance(experience["source_movement_seq"], int), "current experience movement absent"
    assert payload["omission"]["before_current_experience"] is True and payload["omission"]["selected_evidence_trimmed"] is False, "outcome is not bounded to current experience"
    outcome = payload["outcome"]
    assert isinstance(outcome, dict), payload.get("outcome_parse_error") or "outcome must be one JSON object"
    assert set(outcome) == {"schema_version", "status", "source_run", "reason", "receipts"}, "outcome fields mismatch"
    assert outcome["schema_version"] == 1 and outcome["source_run"] == params["source_run"], "outcome source mismatch"
    assert isinstance(outcome["reason"], str) and outcome["reason"].strip(), "outcome reason absent"
    assert outcome["status"] in ("noop", "published"), "capture failed"
    receipts = outcome["receipts"]
    assert isinstance(receipts, list), "outcome receipts must be an array"
    entries = payload["evidence"]
    assert isinstance(entries, list), "outcome context malformed"
    by_ref = {}
    for entry in entries:
        key = reference_key(entry["source_artifact"])
        assert key not in by_ref and isinstance(entry["source_movement_seq"], int) and experience["source_movement_seq"] < entry["source_movement_seq"] < source["terminal_source_movement_seq"], "duplicate or evidence outside current experience"
        by_ref[key] = entry
    calls = {}
    results = {}
    applied = set()
    writer = params["publication_tool"]
    for key, entry in by_ref.items():
        value = entry["payload"]
        is_result = entry["label"] == writer + ".result"
        assert not is_result or isinstance(value, dict), "publication receipt malformed"
        if not isinstance(value, dict) or (value.get("tool") != writer and not is_result):
            continue
        assert value.get("tool") == writer, "publication receipt tool mismatch"
        correlation = value["correlation_id"]
        assert isinstance(correlation, str) and correlation and correlation.strip() == correlation, "publication correlation malformed"
        if entry["label"] == "tool.call" and "arguments" in value:
            assert correlation not in calls, "duplicate publication correlation"
            calls[correlation] = (entry["source_movement_seq"], value["arguments"])
        else:
            assert is_result and value.get("label") == entry["label"], "publication receipt label mismatch"
            assert correlation not in results, "duplicate publication result"
            body = value.get("body")
            results[correlation] = key
            error = value.get("error")
            if value.get("ok") is False and body is None and isinstance(error, dict) and error.get("code") == "tool_invocation_refused":
                continue
            assert value.get("ok") is True and error is None and isinstance(body, dict) and body.get("result") in ("memory_saved", "memory_already_saved"), "publication failed or has an uncertain effect"
            if body["result"] == "memory_saved":
                applied.add(key)
    assert calls.keys() == results.keys(), "publication call has no exact result or result has no call"
    for correlation, key in results.items():
        assert calls[correlation][0] < by_ref[key]["source_movement_seq"], "publication result precedes its call"
    if outcome["status"] == "noop":
        assert not receipts and not applied, "no-op has publication evidence"
        return
    selected = []
    for receipt in receipts:
        assert isinstance(receipt, dict) and set(receipt) == {"tool", "correlation_id"} and receipt["tool"] == writer and isinstance(receipt["correlation_id"], str) and receipt["correlation_id"], "publication receipt selector malformed"
        matches = [key for key in applied if by_ref[key]["payload"].get("correlation_id") == receipt["correlation_id"]]
        assert len(matches) == 1, "publication selector has no unique admitted receipt"
        selected.append(matches[0])
    assert selected and len(set(selected)) == len(selected) and set(selected) == applied, "publication must report every actual writer receipt"
    root = params["entries_root"]
    assert isinstance(root, str) and os.path.isabs(root) and os.path.normpath(root) == root, "publication root malformed"
    for key in selected:
        body = by_ref[key]["payload"]["body"]
        memory, withdrawn, writes = body["memory"], body["withdrawn"], body["writes"]
        assert isinstance(memory, dict) and memory["scope_ee_ids"] == [observed_ee], "publication scope differs from the observed EE"
        assert isinstance(withdrawn, list) and isinstance(writes, list) and len(writes) == 1 + len(withdrawn), "publication receipt count mismatch"
        assert os.path.basename(writes[0]["path"]) == memory["id"] + ".json", "publication receipt names another entry"
        for receipt in writes:
            path = receipt["path"]
            assert isinstance(path, str) and os.path.dirname(path) == root and path.endswith(".json"), "publication path escaped selected entries"
            assert receipt["outcome"] == "applied" and receipt["error"] is None and receipt["after_hash"] == receipt["observed_hash"], "publication was not applied"
            assert isinstance(receipt["after_hash"], str) and re.fullmatch(r"[0-9a-f]{64}", receipt["after_hash"]), "publication receipt hash mismatch"

if __name__ == "__main__":
    try:
        verify(json.load(sys.stdin))
        result = {"passed": True, "details": {"contract": "memory_capture_outcome"}}
    except (AssertionError, KeyError, TypeError, ValueError, OSError) as error:
        result = {"passed": False, "details": {"contract": "memory_capture_outcome", "error": str(error)}}
    print(json.dumps(result))
