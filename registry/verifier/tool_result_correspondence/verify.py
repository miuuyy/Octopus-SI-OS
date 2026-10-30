#!/usr/bin/env python3
import json
import math
import sys


def object_pairs(pairs):
    value = {}
    for key, item in pairs:
        if key in value:
            raise ValueError("duplicate JSON field")
        value[key] = item
    return value


def finite_float(text):
    value = float(text)
    if not math.isfinite(value):
        raise ValueError("non-finite JSON number")
    return value


def invalid_constant(_):
    raise ValueError("non-finite JSON number")


def decode(text):
    return json.loads(text, object_pairs_hook=object_pairs, parse_float=finite_float,
                      parse_constant=invalid_constant)


def text(value):
    return isinstance(value, str) and bool(value) and value.strip() == value


def artifact_ref(value):
    assert isinstance(value, dict) and set(value) == {"id", "content"} and text(value["id"]), "artifact reference is malformed"
    content = value["content"]
    assert isinstance(content, list) and len(content) == 32 and all(type(x) is int and 0 <= x <= 255 for x in content), "artifact reference has invalid content"


def read_binding(metadata, reference):
    artifact_ref(reference)
    bindings = metadata["artifact_binding_refs"]
    assert isinstance(bindings, list) and [x for x in bindings if x.get("id") == reference["id"]] == [reference], "artifact is not bound exactly once"
    access = metadata["grant"]["artifact_access"][reference["id"]]
    assert isinstance(access, dict) and all(type(x) is bool for x in access.values()), "artifact rights are malformed"
    assert access == {"read": True, "change": False, "create": False, "delete": False}, "artifact binding must be read-only"


def verify(request):
    params = request["params"]
    fields = {"call_artifact", "tool", "correlation_id", "params"}
    assert isinstance(params, dict) and fields <= set(params) <= fields | {"invocation"}, "correspondence parameters are malformed"
    assert params["params"] == {}, "unsupported correspondence parameters"
    tool, correlation = params["tool"], params["correlation_id"]
    assert text(tool) and text(correlation), "tool and correlation must be explicit"
    call = params["call_artifact"]
    metadata = request["metadata"]
    read_binding(metadata, call)
    if "invocation" in params:
        invocation = params["invocation"]
        assert isinstance(invocation, dict) and set(invocation) == {"source", "artifact", "manifest_relative_path"}, "invocation declaration is malformed"
        source = invocation["source"]
        assert isinstance(source, dict) and set(source) == {"artifact", "source_movement_seq"}, "invocation source is malformed"
        assert type(source["source_movement_seq"]) is int and 0 <= source["source_movement_seq"] < 2**64, "invocation source sequence is malformed"
        read_binding(metadata, source["artifact"])
        carrier = invocation["artifact"]
        artifact_ref(carrier)
        assert carrier != source["artifact"] and carrier != call, "invocation carrier must be distinct"
        assert request["artifact"] == {"id": carrier["id"], "content": bytes(carrier["content"]).hex()}, "invocation carrier differs from the checked declaration"
        for reference in (source["artifact"], carrier):
            assert reference["id"] == request["artifact_label"] + ":" + bytes(reference["content"]).hex(), "invocation result identity is malformed"
        path = invocation["manifest_relative_path"]
        assert text(path) and all(part and part not in (".", "..") for part in path.split("/")), "invocation carrier path is not private-relative"
    result = decode(request["payload_utf8"])
    assert isinstance(result, dict), "result shape is malformed"
    if "operation" in result:
        verify_operation_result(request, result, tool, correlation)
        return
    assert {"label", "tool", "correlation_id", "ok"} <= set(result) <= {"label", "tool", "correlation_id", "ok", "body", "error"}, "result shape is malformed"
    assert result["tool"] == tool and result["correlation_id"] == correlation, "result answers a different call"
    assert result["label"] == request["artifact_label"] == tool + ".result", "result label differs from its tool"
    if result["ok"] is True:
        assert "body" in result and result["body"] is not None and "error" not in result, "success result has inconsistent evidence"
    else:
        assert result["ok"] is False and "body" not in result, "result status is malformed"
        error = result["error"]
        assert isinstance(error, dict) and {"code", "message"} <= set(error) <= {"code", "message", "details"}, "failure result is malformed"
        assert text(error["code"]) and text(error["message"]), "failure result has empty details"
        assert "details" not in error or isinstance(error["details"], dict) and error["details"], "failure details must be nonempty"


def verify_operation_result(request, result, operation, correlation):
    assert {"label", "operation", "correlation_id", "ok"} <= set(result), "operation result shape is malformed"
    assert result["operation"] == operation and result["correlation_id"] == correlation, "result answers a different call"
    assert text(result["label"]) and result["label"] == request["artifact_label"], "result label differs from its declared result"
    assert type(result["ok"]) is bool, "result status is malformed"
    assert ("result" in result) == result["ok"] and ("error" in result) != result["ok"], "operation result has inconsistent evidence"


def main():
    try:
        verify(decode(sys.stdin.read()))
        verdict = {"passed": True, "details": {"correspondence": "exact"}}
    except (AssertionError, AttributeError, KeyError, TypeError, ValueError) as error:
        verdict = {"passed": False, "details": {"reason": str(error)}}
    print(json.dumps(verdict, separators=(",", ":")))


if __name__ == "__main__":
    main()
