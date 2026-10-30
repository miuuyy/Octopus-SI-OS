import json
import os
import re
import stat


DESCRIPTOR_KEYS = {"id", "request_label", "output_label", "handler_config", "description", "input_schema"}
REQUEST_KEYS = {"operation", "arguments", "correlation_id"}
CATALOG_KEYS = ("operation_catalog_dir", "operation_catalog_label", "operation_request_label")
SCHEMA_KEYWORDS = {"type", "description", "title", "properties", "required", "additionalProperties", "items", "minItems", "maxItems", "minLength", "maxLength", "minimum", "maximum", "enum", "const", "pattern", "anyOf", "default", "examples"}
TYPES = {"object", "array", "string", "integer", "number", "boolean", "null"}
MAX_DESCRIPTOR_BYTES = 262144
MAX_CORRELATION_CHARS = 512


class RequestError(ValueError):
    def __init__(self, code, message, operation=None, correlation_id=None):
        super().__init__(message)
        self.code = code
        self.operation = operation
        self.correlation_id = correlation_id


def trimmed(value):
    return isinstance(value, str) and bool(value) and value.strip() == value


def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False)


def read_descriptor(path):
    descriptor = os.open(path, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0))
    with os.fdopen(descriptor, "rb") as handle:
        if not stat.S_ISREG(os.fstat(handle.fileno()).st_mode):
            raise ValueError(f"operation descriptor {path} is not a regular file")
        data = handle.read(MAX_DESCRIPTOR_BYTES + 1)
    if len(data) > MAX_DESCRIPTOR_BYTES:
        raise ValueError(f"operation descriptor {path} exceeds {MAX_DESCRIPTOR_BYTES} bytes")
    return json.loads(data)


def check_schema(schema, path):
    if not isinstance(schema, dict):
        raise ValueError(f"{path} must be a JSON object")
    unknown = sorted(set(schema) - SCHEMA_KEYWORDS)
    if unknown:
        raise ValueError(f"{path} uses unsupported schema keywords: {', '.join(unknown)}")
    if "type" in schema:
        kinds = schema["type"] if isinstance(schema["type"], list) else [schema["type"]]
        if not kinds or any(kind not in TYPES for kind in kinds):
            raise ValueError(f"{path}.type names an unknown type")
    properties = schema.get("properties", {})
    if not isinstance(properties, dict):
        raise ValueError(f"{path}.properties must be an object")
    for name, child in properties.items():
        check_schema(child, f"{path}.properties.{name}")
    required = schema.get("required", [])
    if not isinstance(required, list) or any(not trimmed(name) for name in required) or len(set(required)) != len(required):
        raise ValueError(f"{path}.required must list distinct property names")
    if "properties" in schema and not set(required) <= set(properties):
        raise ValueError(f"{path}.required names an undeclared property")
    if "additionalProperties" in schema and not isinstance(schema["additionalProperties"], bool):
        raise ValueError(f"{path}.additionalProperties must be a boolean")
    if "items" in schema:
        check_schema(schema["items"], f"{path}.items")
    for key in ("minItems", "maxItems", "minLength", "maxLength"):
        if key in schema and (isinstance(schema[key], bool) or not isinstance(schema[key], int) or schema[key] < 0):
            raise ValueError(f"{path}.{key} must be a non-negative integer")
    for key in ("minimum", "maximum"):
        if key in schema and (isinstance(schema[key], bool) or not isinstance(schema[key], (int, float))):
            raise ValueError(f"{path}.{key} must be a number")
    if "enum" in schema and (not isinstance(schema["enum"], list) or not schema["enum"]):
        raise ValueError(f"{path}.enum must be a non-empty array")
    if "pattern" in schema:
        if not isinstance(schema["pattern"], str):
            raise ValueError(f"{path}.pattern must be a string")
        re.compile(schema["pattern"])
    if "anyOf" in schema:
        if not isinstance(schema["anyOf"], list) or not schema["anyOf"]:
            raise ValueError(f"{path}.anyOf must be a non-empty array")
        for index, child in enumerate(schema["anyOf"]):
            check_schema(child, f"{path}.anyOf[{index}]")
    for key in ("description", "title"):
        if key in schema and not isinstance(schema[key], str):
            raise ValueError(f"{path}.{key} must be a string")


def has_type(kind, value):
    if kind == "object":
        return isinstance(value, dict)
    if kind == "array":
        return isinstance(value, list)
    if kind == "string":
        return isinstance(value, str)
    if kind == "integer":
        return isinstance(value, int) and not isinstance(value, bool)
    if kind == "number":
        return isinstance(value, (int, float)) and not isinstance(value, bool)
    if kind == "boolean":
        return isinstance(value, bool)
    return value is None


def validate(schema, value, path="arguments"):
    if "type" in schema:
        kinds = schema["type"] if isinstance(schema["type"], list) else [schema["type"]]
        if not any(has_type(kind, value) for kind in kinds):
            raise ValueError(f"{path} must be {' or '.join(kinds)}")
    if "const" in schema and canonical(value) != canonical(schema["const"]):
        raise ValueError(f"{path} must be {canonical(schema['const'])}")
    if "enum" in schema and canonical(value) not in {canonical(item) for item in schema["enum"]}:
        raise ValueError(f"{path} must be one of {', '.join(canonical(item) for item in schema['enum'])}")
    if isinstance(value, str):
        if len(value) < schema.get("minLength", 0) or len(value) > schema.get("maxLength", len(value)):
            raise ValueError(f"{path} must have {schema.get('minLength', 0)} to {schema.get('maxLength', 'any number of')} characters")
        if "pattern" in schema and not re.search(schema["pattern"], value):
            raise ValueError(f"{path} does not match {schema['pattern']}")
    if has_type("number", value):
        if value < schema.get("minimum", value) or value > schema.get("maximum", value):
            raise ValueError(f"{path} must be between {schema.get('minimum', '-∞')} and {schema.get('maximum', '∞')}")
    if isinstance(value, list):
        if len(value) < schema.get("minItems", 0) or len(value) > schema.get("maxItems", len(value)):
            raise ValueError(f"{path} must hold {schema.get('minItems', 0)} to {schema.get('maxItems', 'any number of')} items")
        if "items" in schema:
            for index, item in enumerate(value):
                validate(schema["items"], item, f"{path}[{index}]")
    if isinstance(value, dict):
        properties = schema.get("properties", {})
        missing = [name for name in schema.get("required", []) if name not in value]
        if missing:
            raise ValueError(f"{path} is missing {', '.join(missing)}")
        if schema.get("additionalProperties") is False:
            unknown = sorted(set(value) - set(properties))
            if unknown:
                raise ValueError(f"{path} has unknown properties {', '.join(unknown)}")
        for name, child in properties.items():
            if name in value:
                validate(child, value[name], f"{path}.{name}")
    if "anyOf" in schema:
        errors = []
        for child in schema["anyOf"]:
            try:
                validate(child, value, path)
                break
            except ValueError as error:
                errors.append(str(error))
        else:
            raise ValueError(f"{path} matches none of its allowed forms: {'; '.join(errors)}")


def load_catalog(interface_dir, private_config, prefix):
    if not isinstance(private_config, dict):
        raise ValueError("interface private_config must be an object")
    names = {}
    for key in CATALOG_KEYS:
        if not trimmed(private_config.get(key)):
            raise ValueError(f"interface private_config.{key} is absent or malformed")
        names[key] = private_config[key]
    relative = names["operation_catalog_dir"]
    if os.path.isabs(relative) or any(part in {"", ".", ".."} for part in relative.split("/")):
        raise ValueError("operation_catalog_dir must be a normalized relative path")
    root = os.path.join(interface_dir, relative)
    status = os.lstat(root)
    if stat.S_ISLNK(status.st_mode) or not stat.S_ISDIR(status.st_mode):
        raise ValueError(f"operation catalog {root} is not a directory")
    operations = {}
    output_labels = set()
    for name in sorted(os.listdir(root)):
        path = os.path.join(root, name)
        if not name.endswith(".json") or not stat.S_ISREG(os.lstat(path).st_mode):
            raise ValueError(f"operation catalog entry {name} is not a regular JSON file")
        entry = read_descriptor(path)
        if not isinstance(entry, dict) or set(entry) != DESCRIPTOR_KEYS:
            raise ValueError(f"operation {name} must carry exactly {', '.join(sorted(DESCRIPTOR_KEYS))}")
        for key in ("id", "request_label", "output_label", "description"):
            if not trimmed(entry[key]):
                raise ValueError(f"operation {name} has a malformed {key}")
        identity = entry["id"]
        if f"{identity}.json" != name or not identity.startswith(f"{prefix}."):
            raise ValueError(f"operation {identity} must be named {prefix}.<action> and live in {identity}.json")
        if entry["request_label"] != names["operation_request_label"]:
            raise ValueError(f"operation {identity} request_label differs from {names['operation_request_label']}")
        if not isinstance(entry["handler_config"], dict) or not trimmed(entry["handler_config"].get("operation")):
            raise ValueError(f"operation {identity} handler_config must name its handler operation")
        schema = entry["input_schema"]
        check_schema(schema, f"{identity}.input_schema")
        if schema.get("type") != "object" or schema.get("additionalProperties") is not False or not isinstance(schema.get("properties"), dict):
            raise ValueError(f"operation {identity} input_schema must be a closed object with declared properties")
        output_labels.add(entry["output_label"])
        operations[identity] = entry
    if not operations:
        raise ValueError(f"operation catalog {root} is empty")
    if len(output_labels) != 1:
        raise ValueError("every operation of one interface answers on the same output_label")
    return {
        "catalog_label": names["operation_catalog_label"],
        "request_label": names["operation_request_label"],
        "result_label": output_labels.pop(),
        "operations": operations,
    }


def parse_request(payload, catalog):
    try:
        body = json.loads(payload)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise RequestError("malformed_request", f"the operation request is not JSON: {error}") from None
    if not isinstance(body, dict):
        raise RequestError("malformed_request", "the operation request must be a JSON object")
    operation = body.get("operation") if trimmed(body.get("operation")) else None
    correlation_id = body.get("correlation_id")
    correlation_id = correlation_id if trimmed(correlation_id) and len(correlation_id) <= MAX_CORRELATION_CHARS else None
    if set(body) != REQUEST_KEYS:
        raise RequestError("malformed_request", "the operation request carries exactly operation, arguments and correlation_id", operation, correlation_id)
    if correlation_id is None:
        raise RequestError("malformed_request", f"correlation_id must be a trimmed string of 1 to {MAX_CORRELATION_CHARS} characters", operation)
    if operation is None:
        raise RequestError("malformed_request", "operation must name an operation of this interface", None, correlation_id)
    spec = catalog["operations"].get(operation)
    if spec is None:
        raise RequestError("unknown_operation", f"{operation} is not in this interface's operation catalog", operation, correlation_id)
    try:
        validate(spec["input_schema"], body["arguments"])
    except ValueError as error:
        raise RequestError("invalid_arguments", str(error), operation, correlation_id) from None
    return spec, dict(body["arguments"]), correlation_id


def answer(label, operation, correlation_id, result=None, error=None, media=None):
    body = {"label": label, "operation": operation, "correlation_id": correlation_id, "ok": error is None}
    if error is None:
        body["result"] = result
    else:
        body["error"] = error
    if media:
        body["media"] = media
    return body
