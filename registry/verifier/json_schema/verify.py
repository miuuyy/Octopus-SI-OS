#!/usr/bin/env python3
"""Deterministic strict JSON Schema subset verifier for Octopus artifacts."""

import json
import math
import os
import stat
import sys

HANDLER = "json_schema"
DEFAULT_MAX_PAYLOAD_BYTES = 4 * 1024 * 1024
MAX_ALLOWED_PAYLOAD_BYTES = 16 * 1024 * 1024
DEFAULT_MAX_DEPTH = 32
MAX_ALLOWED_DEPTH = 64

ALLOWED_PARAM_KEYS = {"schema", "max_payload_bytes", "max_depth", "timeout_ms"}
ALLOWED_SCHEMA_KEYWORDS = {
    "title",
    "description",
    "$schema",
    "type",
    "enum",
    "const",
    "required",
    "properties",
    "additionalProperties",
    "minProperties",
    "maxProperties",
    "items",
    "minItems",
    "maxItems",
    "uniqueItems",
    "minLength",
    "maxLength",
    "minimum",
    "maximum",
    "exclusiveMinimum",
    "exclusiveMaximum",
    "allOf",
    "anyOf",
    "oneOf",
}
VALID_JSON_TYPES = {
    "object",
    "array",
    "string",
    "integer",
    "number",
    "boolean",
    "null",
}


class SchemaError(Exception):
    def __init__(self, reason, **details):
        super().__init__(reason)
        self.reason = reason
        self.details = details


class ValidationFailure(Exception):
    def __init__(self, path, keyword, message):
        super().__init__(message)
        self.path = path
        self.keyword = keyword
        self.message = message


class DepthExceeded(Exception):
    def __init__(self, path):
        super().__init__(path)
        self.path = path


def verdict(passed, details):
    print(json.dumps({"passed": passed, "details": details}, separators=(",", ":")))


def fail(reason, **details):
    verdict(False, {"handler": HANDLER, "reason": reason, **details})


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


def strict_json(text):
    return json.loads(
        text,
        object_pairs_hook=unique_object,
        parse_constant=invalid_constant,
        parse_float=finite_float,
    )


def is_strict_int(value):
    return type(value) is int


def is_finite_number(value):
    if type(value) is int:
        return True
    if type(value) is float:
        return math.isfinite(value)
    return False


def escape_pointer_segment(segment):
    return str(segment).replace("~", "~0").replace("/", "~1")


def join_pointer(base, segment):
    return f"{base}/{escape_pointer_segment(segment)}"


def json_type_of(value):
    if value is None:
        return "null"
    if type(value) is bool:
        return "boolean"
    if type(value) is int:
        return "integer"
    if type(value) is float:
        return "number"
    if type(value) is str:
        return "string"
    if type(value) is list:
        return "array"
    if type(value) is dict:
        return "object"
    return "unknown"


def matches_type(value, expected_type):
    if expected_type == "null":
        return value is None
    if expected_type == "boolean":
        return type(value) is bool
    if expected_type == "integer":
        return type(value) is int
    if expected_type == "number":
        return is_finite_number(value)
    if expected_type == "string":
        return type(value) is str
    if expected_type == "array":
        return type(value) is list
    if expected_type == "object":
        return type(value) is dict
    return False


def canonical_key(value):
    """Canonical hashable key preserving JSON type distinctions (True != 1)."""
    if value is None:
        return ("null", None)
    if type(value) is bool:
        return ("bool", value)
    if type(value) is int:
        return ("num", value)
    if type(value) is float:
        if value.is_integer():
            return ("num", int(value))
        return ("num", value)
    if type(value) is str:
        return ("str", value)
    if type(value) is list:
        return ("arr", tuple(canonical_key(item) for item in value))
    if type(value) is dict:
        return (
            "obj",
            tuple(sorted((k, canonical_key(v)) for k, v in value.items())),
        )
    raise TypeError(f"unsupported JSON value type: {type(value).__name__}")


def json_equal(left, right):
    return canonical_key(left) == canonical_key(right)


def compile_schema(schema, max_depth, depth=0, path=""):
    if depth > max_depth:
        raise SchemaError("max_depth_exceeded", path=path or "/", scope="schema")
    if not isinstance(schema, dict):
        raise SchemaError("schema_invalid", path=path or "/", message="schema must be an object")

    for keyword in sorted(schema):
        if keyword not in ALLOWED_SCHEMA_KEYWORDS:
            raise SchemaError(
                "schema_keyword_unsupported",
                path=path or "/",
                keyword=keyword,
            )

    compiled = {}

    for meta_key in ("title", "description", "$schema"):
        if meta_key in schema:
            if not isinstance(schema[meta_key], str):
                raise SchemaError(
                    "schema_invalid",
                    path=join_pointer(path, meta_key),
                    message=f"{meta_key} must be a string",
                )

    if "type" in schema:
        raw_type = schema["type"]
        if isinstance(raw_type, str):
            if raw_type not in VALID_JSON_TYPES:
                raise SchemaError(
                    "schema_invalid",
                    path=join_pointer(path, "type"),
                    message=f"unknown JSON type: {raw_type}",
                )
            compiled["type"] = (raw_type,)
        elif isinstance(raw_type, list) and raw_type:
            seen = set()
            types = []
            for item in raw_type:
                if not isinstance(item, str) or item not in VALID_JSON_TYPES:
                    raise SchemaError(
                        "schema_invalid",
                        path=join_pointer(path, "type"),
                        message="type array entries must be valid JSON type names",
                    )
                if item in seen:
                    raise SchemaError(
                        "schema_invalid",
                        path=join_pointer(path, "type"),
                        message="type array must contain unique entries",
                    )
                seen.add(item)
                types.append(item)
            compiled["type"] = tuple(types)
        else:
            raise SchemaError(
                "schema_invalid",
                path=join_pointer(path, "type"),
                message="type must be a type name or non-empty array of unique type names",
            )

    if "enum" in schema:
        raw_enum = schema["enum"]
        if not isinstance(raw_enum, list) or not raw_enum:
            raise SchemaError(
                "schema_invalid",
                path=join_pointer(path, "enum"),
                message="enum must be a non-empty array",
            )
        compiled["enum"] = raw_enum

    if "const" in schema:
        compiled["const"] = (True, schema["const"])

    if "required" in schema:
        raw_required = schema["required"]
        if not isinstance(raw_required, list):
            raise SchemaError(
                "schema_invalid",
                path=join_pointer(path, "required"),
                message="required must be an array of unique strings",
            )
        seen_req = set()
        for item in raw_required:
            if not isinstance(item, str) or item in seen_req:
                raise SchemaError(
                    "schema_invalid",
                    path=join_pointer(path, "required"),
                    message="required must contain unique strings",
                )
            seen_req.add(item)
        compiled["required"] = tuple(raw_required)

    if "properties" in schema:
        raw_props = schema["properties"]
        if not isinstance(raw_props, dict):
            raise SchemaError(
                "schema_invalid",
                path=join_pointer(path, "properties"),
                message="properties must be an object",
            )
        compiled_props = {}
        props_ptr = join_pointer(path, "properties")
        for prop_name, prop_schema in raw_props.items():
            compiled_props[prop_name] = compile_schema(
                prop_schema,
                max_depth,
                depth + 1,
                join_pointer(props_ptr, prop_name),
            )
        compiled["properties"] = compiled_props

    if "additionalProperties" in schema:
        raw_add = schema["additionalProperties"]
        if type(raw_add) is bool:
            compiled["additionalProperties"] = raw_add
        elif isinstance(raw_add, dict):
            compiled["additionalProperties"] = compile_schema(
                raw_add,
                max_depth,
                depth + 1,
                join_pointer(path, "additionalProperties"),
            )
        else:
            raise SchemaError(
                "schema_invalid",
                path=join_pointer(path, "additionalProperties"),
                message="additionalProperties must be a boolean or schema object",
            )

    for bound_key in ("minProperties", "maxProperties", "minItems", "maxItems", "minLength", "maxLength"):
        if bound_key in schema:
            val = schema[bound_key]
            if not is_strict_int(val) or val < 0:
                raise SchemaError(
                    "schema_invalid",
                    path=join_pointer(path, bound_key),
                    message=f"{bound_key} must be a non-negative integer",
                )
            compiled[bound_key] = val

    for lo_key, hi_key in (
        ("minProperties", "maxProperties"),
        ("minItems", "maxItems"),
        ("minLength", "maxLength"),
    ):
        if lo_key in compiled and hi_key in compiled and compiled[lo_key] > compiled[hi_key]:
            raise SchemaError(
                "schema_invalid",
                path=path or "/",
                message=f"{lo_key} cannot exceed {hi_key}",
            )

    if "items" in schema:
        compiled["items"] = compile_schema(
            schema["items"],
            max_depth,
            depth + 1,
            join_pointer(path, "items"),
        )

    if "uniqueItems" in schema:
        if type(schema["uniqueItems"]) is not bool:
            raise SchemaError(
                "schema_invalid",
                path=join_pointer(path, "uniqueItems"),
                message="uniqueItems must be a boolean",
            )
        compiled["uniqueItems"] = schema["uniqueItems"]

    for num_key in ("minimum", "maximum", "exclusiveMinimum", "exclusiveMaximum"):
        if num_key in schema:
            val = schema[num_key]
            if not is_finite_number(val):
                raise SchemaError(
                    "schema_invalid",
                    path=join_pointer(path, num_key),
                    message=f"{num_key} must be a finite number",
                )
            compiled[num_key] = val

    if "minimum" in compiled and "maximum" in compiled and compiled["minimum"] > compiled["maximum"]:
        raise SchemaError(
            "schema_invalid",
            path=path or "/",
            message="minimum cannot exceed maximum",
        )

    for comb_key in ("allOf", "anyOf", "oneOf"):
        if comb_key in schema:
            raw_list = schema[comb_key]
            if not isinstance(raw_list, list) or not raw_list:
                raise SchemaError(
                    "schema_invalid",
                    path=join_pointer(path, comb_key),
                    message=f"{comb_key} must be a non-empty array of schemas",
                )
            comb_ptr = join_pointer(path, comb_key)
            compiled[comb_key] = [
                compile_schema(sub, max_depth, depth + 1, join_pointer(comb_ptr, idx))
                for idx, sub in enumerate(raw_list)
            ]

    return compiled


def check_instance_depth(value, max_depth, depth=0, path=""):
    if depth > max_depth:
        raise DepthExceeded(path or "/")
    if type(value) is dict:
        for key, child in value.items():
            check_instance_depth(child, max_depth, depth + 1, join_pointer(path, key))
    elif type(value) is list:
        for idx, child in enumerate(value):
            check_instance_depth(child, max_depth, depth + 1, join_pointer(path, idx))


def validate_instance(value, compiled, path=""):
    if "type" in compiled:
        allowed_types = compiled["type"]
        if not any(matches_type(value, expected) for expected in allowed_types):
            raise ValidationFailure(
                path or "/",
                "type",
                f"expected type {'|'.join(allowed_types)}, got {json_type_of(value)}",
            )

    if "const" in compiled:
        _, expected = compiled["const"]
        if not json_equal(value, expected):
            raise ValidationFailure(path or "/", "const", "value does not match const")

    if "enum" in compiled:
        if not any(json_equal(value, candidate) for candidate in compiled["enum"]):
            raise ValidationFailure(path or "/", "enum", "value is not in enum")

    if type(value) is dict:
        if "minProperties" in compiled and len(value) < compiled["minProperties"]:
            raise ValidationFailure(
                path or "/",
                "minProperties",
                f"object has {len(value)} properties, minimum is {compiled['minProperties']}",
            )
        if "maxProperties" in compiled and len(value) > compiled["maxProperties"]:
            raise ValidationFailure(
                path or "/",
                "maxProperties",
                f"object has {len(value)} properties, maximum is {compiled['maxProperties']}",
            )
        if "required" in compiled:
            for req_key in compiled["required"]:
                if req_key not in value:
                    raise ValidationFailure(
                        join_pointer(path, req_key),
                        "required",
                        f"missing required property '{req_key}'",
                    )
        props = compiled.get("properties", {})
        for prop_name, prop_schema in props.items():
            if prop_name in value:
                validate_instance(value[prop_name], prop_schema, join_pointer(path, prop_name))
        if "additionalProperties" in compiled:
            add_rule = compiled["additionalProperties"]
            for key in sorted(value):
                if key in props:
                    continue
                if add_rule is False:
                    raise ValidationFailure(
                        join_pointer(path, key),
                        "additionalProperties",
                        f"additional property '{key}' is not allowed",
                    )
                if isinstance(add_rule, dict):
                    validate_instance(value[key], add_rule, join_pointer(path, key))

    elif type(value) is list:
        if "minItems" in compiled and len(value) < compiled["minItems"]:
            raise ValidationFailure(
                path or "/",
                "minItems",
                f"array has {len(value)} items, minimum is {compiled['minItems']}",
            )
        if "maxItems" in compiled and len(value) > compiled["maxItems"]:
            raise ValidationFailure(
                path or "/",
                "maxItems",
                f"array has {len(value)} items, maximum is {compiled['maxItems']}",
            )
        if compiled.get("uniqueItems"):
            seen = set()
            for idx, item in enumerate(value):
                key = canonical_key(item)
                if key in seen:
                    raise ValidationFailure(
                        join_pointer(path, idx),
                        "uniqueItems",
                        "array items must be unique",
                    )
                seen.add(key)
        if "items" in compiled:
            item_schema = compiled["items"]
            for idx, item in enumerate(value):
                validate_instance(item, item_schema, join_pointer(path, idx))

    elif type(value) is str:
        if "minLength" in compiled and len(value) < compiled["minLength"]:
            raise ValidationFailure(
                path or "/",
                "minLength",
                f"string length {len(value)} is less than minLength {compiled['minLength']}",
            )
        if "maxLength" in compiled and len(value) > compiled["maxLength"]:
            raise ValidationFailure(
                path or "/",
                "maxLength",
                f"string length {len(value)} exceeds maxLength {compiled['maxLength']}",
            )

    elif is_finite_number(value):
        if "minimum" in compiled and value < compiled["minimum"]:
            raise ValidationFailure(
                path or "/",
                "minimum",
                f"value {value} is less than minimum {compiled['minimum']}",
            )
        if "maximum" in compiled and value > compiled["maximum"]:
            raise ValidationFailure(
                path or "/",
                "maximum",
                f"value {value} exceeds maximum {compiled['maximum']}",
            )
        if "exclusiveMinimum" in compiled and value <= compiled["exclusiveMinimum"]:
            raise ValidationFailure(
                path or "/",
                "exclusiveMinimum",
                f"value {value} must be greater than {compiled['exclusiveMinimum']}",
            )
        if "exclusiveMaximum" in compiled and value >= compiled["exclusiveMaximum"]:
            raise ValidationFailure(
                path or "/",
                "exclusiveMaximum",
                f"value {value} must be less than {compiled['exclusiveMaximum']}",
            )

    if "allOf" in compiled:
        for sub in compiled["allOf"]:
            validate_instance(value, sub, path)

    if "anyOf" in compiled:
        matched = False
        for sub in compiled["anyOf"]:
            try:
                validate_instance(value, sub, path)
                matched = True
                break
            except ValidationFailure:
                continue
        if not matched:
            raise ValidationFailure(path or "/", "anyOf", "value matched none of anyOf subschemas")

    if "oneOf" in compiled:
        matches = 0
        for sub in compiled["oneOf"]:
            try:
                validate_instance(value, sub, path)
                matches += 1
            except ValidationFailure:
                continue
        if matches != 1:
            raise ValidationFailure(
                path or "/",
                "oneOf",
                f"value matched {matches} of oneOf subschemas (expected 1)",
            )


def read_payload_bytes(payload_path, max_payload_bytes):
    if not isinstance(payload_path, str) or not payload_path:
        return None, ("payload_unavailable", {"message": "payload_path is missing"})
    try:
        lstat_info = os.lstat(payload_path)
    except OSError as err:
        return None, ("payload_unavailable", {"message": str(err)})
    if stat.S_ISLNK(lstat_info.st_mode) or not stat.S_ISREG(lstat_info.st_mode):
        return None, ("payload_unavailable", {"message": "payload_path is not a regular file"})
    if lstat_info.st_size > max_payload_bytes:
        return None, (
            "payload_too_large",
            {"payload_bytes": lstat_info.st_size, "max_payload_bytes": max_payload_bytes},
        )
    flags = os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0)
    try:
        fd = os.open(payload_path, flags)
    except OSError as err:
        return None, ("payload_unavailable", {"message": str(err)})
    try:
        fstat_info = os.fstat(fd)
        if not stat.S_ISREG(fstat_info.st_mode):
            return None, ("payload_unavailable", {"message": "payload descriptor is not a regular file"})
        chunks = []
        total = 0
        with os.fdopen(fd, "rb", closefd=False) as handle:
            while True:
                chunk = handle.read(65536)
                if not chunk:
                    break
                total += len(chunk)
                if total > max_payload_bytes:
                    return None, (
                        "payload_too_large",
                        {"payload_bytes": total, "max_payload_bytes": max_payload_bytes},
                    )
                chunks.append(chunk)
        return b"".join(chunks), None
    except OSError as err:
        return None, ("payload_unavailable", {"message": str(err)})
    finally:
        os.close(fd)


def main():
    try:
        request = strict_json(sys.stdin.read())
    except Exception as err:
        fail("request_json_invalid", error=str(err))
        return

    if not isinstance(request, dict):
        fail("request_not_object")
        return

    params = request.get("params")
    if not isinstance(params, dict):
        fail("params_not_object")
        return

    keys = set(params)
    if "schema" not in keys or not keys.issubset(ALLOWED_PARAM_KEYS):
        fail("params_shape_invalid", keys=sorted(keys))
        return

    if "timeout_ms" in params:
        timeout_ms = params["timeout_ms"]
        if not is_strict_int(timeout_ms) or timeout_ms < 1:
            fail("params_invalid", field="timeout_ms")
            return

    max_payload_bytes = params.get("max_payload_bytes", DEFAULT_MAX_PAYLOAD_BYTES)
    if not is_strict_int(max_payload_bytes) or not (1 <= max_payload_bytes <= MAX_ALLOWED_PAYLOAD_BYTES):
        fail("params_invalid", field="max_payload_bytes")
        return

    max_depth = params.get("max_depth", DEFAULT_MAX_DEPTH)
    if not is_strict_int(max_depth) or not (1 <= max_depth <= MAX_ALLOWED_DEPTH):
        fail("params_invalid", field="max_depth")
        return

    try:
        compiled_schema = compile_schema(params["schema"], max_depth)
    except SchemaError as err:
        fail(err.reason, **err.details)
        return

    raw_bytes, read_err = read_payload_bytes(request.get("payload_path"), max_payload_bytes)
    if read_err is not None:
        reason, extra = read_err
        fail(reason, **extra)
        return

    try:
        payload_text = raw_bytes.decode("utf-8")
    except UnicodeDecodeError as err:
        fail("payload_not_utf8", error=str(err))
        return

    try:
        instance = strict_json(payload_text)
    except Exception as err:
        fail("payload_json_invalid", error=str(err))
        return

    try:
        check_instance_depth(instance, max_depth)
    except DepthExceeded as err:
        fail("max_depth_exceeded", path=err.path, max_depth=max_depth)
        return

    try:
        validate_instance(instance, compiled_schema)
    except ValidationFailure as err:
        fail("validation_failed", path=err.path, keyword=err.keyword, message=err.message)
        return

    verdict(
        True,
        {
            "handler": HANDLER,
            "payload_bytes": len(raw_bytes),
            "root_type": json_type_of(instance),
        },
    )


if __name__ == "__main__":
    main()
