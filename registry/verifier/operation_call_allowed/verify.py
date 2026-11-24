#!/usr/bin/env python3
import json
import math
import sys

REQUIRED_KEYS = {"operation", "arguments", "correlation_id"}
ALLOWED_KEYS = {"operation", "arguments", "correlation_id", "approval_policy"}


def verdict(passed, details):
    print(json.dumps({"passed": passed, "details": details}, separators=(",", ":")))


def fail(reason, **details):
    verdict(False, {"handler": "operation_call_allowed", "reason": reason, **details})


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError(f"duplicate JSON key: {key}")
        result[key] = value
    return result


def invalid_constant(value):
    raise ValueError(f"non-finite JSON number: {value}")


def finite_float(value):
    result = float(value)
    if not math.isfinite(result):
        invalid_constant(value)
    return result


def strict_json(value):
    return json.loads(
        value,
        object_pairs_hook=unique_object,
        parse_constant=invalid_constant,
        parse_float=finite_float,
    )


def non_empty_trimmed_str(value):
    return isinstance(value, str) and bool(value) and value.strip() == value


def main():
    try:
        request_utf8 = sys.stdin.read()
        request = strict_json(request_utf8)
    except Exception as error:
        fail("request_json_invalid", error=str(error))
        return
    if not isinstance(request, dict):
        fail("request_not_object")
        return
    params = request.get("params")
    if not isinstance(params, dict):
        fail("params_not_object")
        return
    if set(params) != {"operations"}:
        fail("params_shape_invalid", keys=sorted(params))
        return
    operations = params.get("operations")
    if not isinstance(operations, list) or not operations:
        fail("operations_not_nonempty_array")
        return
    if any(not non_empty_trimmed_str(operation) for operation in operations):
        fail("operation_id_invalid")
        return
    if len(set(operations)) != len(operations):
        fail("operation_id_duplicate")
        return
    payload_utf8 = request.get("payload_utf8")
    if not isinstance(payload_utf8, str):
        fail("payload_utf8_not_string")
        return
    try:
        payload = strict_json(payload_utf8)
    except Exception as error:
        fail("payload_json_invalid", error=str(error))
        return
    if not isinstance(payload, dict):
        fail("payload_not_object")
        return
    keys = set(payload)
    if not REQUIRED_KEYS.issubset(keys) or not keys.issubset(ALLOWED_KEYS):
        fail("payload_shape_invalid", keys=sorted(keys))
        return
    operation = payload.get("operation")
    if not non_empty_trimmed_str(operation):
        fail("payload_operation_invalid")
        return
    if not isinstance(payload.get("arguments"), dict):
        fail("payload_arguments_not_object", operation=operation)
        return
    correlation_id = payload.get("correlation_id")
    if not isinstance(correlation_id, str) or not correlation_id.strip():
        fail("payload_correlation_id_invalid", operation=operation)
        return
    if "approval_policy" in payload:
        approval_policy = payload.get("approval_policy")
        if not isinstance(approval_policy, str) or not approval_policy.strip():
            fail("payload_approval_policy_invalid", operation=operation)
            return
    if operation not in operations:
        fail("operation_not_allowed", operation=operation, operations=operations)
        return
    verdict(
        True,
        {
            "handler": "operation_call_allowed",
            "operation": operation,
        },
    )


if __name__ == "__main__":
    main()
