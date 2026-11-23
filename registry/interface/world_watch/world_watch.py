"""JSON-configured read-only World sensor: HTTP/file polling and local webhook
push, deterministic extraction and trigger evaluation, durable SQLite outbox."""
import datetime as dt
import hashlib
import hmac
import http.server
import ipaddress
import json
import math
import os
import re
import socket
import sqlite3
import threading
import time
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path


DEFAULT_LABEL = "watch.event"
LABEL_PATTERN = re.compile(r"^[a-z0-9][a-z0-9._-]{0,127}$")
ENV_NAME_PATTERN = re.compile(r"^[A-Z_][A-Z0-9_]*$")
HEADER_NAME_PATTERN = re.compile(r"^[A-Za-z0-9-]+$")
FORBIDDEN_STATIC_HEADERS = frozenset({"authorization", "proxy-authorization"})
MAX_STATE_BYTES = 16 * 1024 * 1024
MAX_VALUE_JSON_BYTES = 32 * 1024
MAX_EXCERPT_CHARS = 4096
MAX_PENDING_NOTICES = 64
DEFAULT_MAX_RESPONSE_BYTES = 262144
DEFAULT_TIMEOUT_SECONDS = 15
DEFAULT_WEBHOOK_POLL_SECONDS = 60
HOLD_RETRY_SECONDS = 1.0


class ProbeError(RuntimeError):
    def __init__(self, code, detail="", retry_at=None):
        super().__init__(f"{code}: {detail}" if detail else code)
        self.code = code
        self.detail = detail[:1024]
        self.retry_at = retry_at


def now_iso():
    return dt.datetime.now(dt.timezone.utc).isoformat(timespec="seconds").replace("+00:00", "Z")


def canonical(value):
    return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode("utf-8")


def _is_number(value):
    return isinstance(value, (int, float)) and not isinstance(value, bool) and math.isfinite(value)


def _strict_equal(left, right):
    if type(left) is bool or type(right) is bool:
        return type(left) is type(right) and left == right
    if _is_number(left) and _is_number(right):
        return left == right
    if isinstance(left, list) and isinstance(right, list):
        return len(left) == len(right) and all(_strict_equal(a, b) for a, b in zip(left, right))
    if isinstance(left, dict) and isinstance(right, dict):
        return left.keys() == right.keys() and all(_strict_equal(left[k], right[k]) for k in left)
    return type(left) is type(right) and left == right


def _require_keys(value, required, optional, name):
    if not isinstance(value, dict):
        raise ValueError(f"{name} must be a JSON object")
    missing = sorted(set(required) - set(value))
    unknown = sorted(set(value) - set(required) - set(optional))
    if missing:
        raise ValueError(f"{name} is missing keys: {', '.join(missing)}")
    if unknown:
        raise ValueError(f"{name} has unknown keys: {', '.join(unknown)}")
    return value


def _require_bool(value, name):
    if not isinstance(value, bool):
        raise ValueError(f"{name} must be a boolean")
    return value


def _require_int_range(value, name, minimum, maximum):
    if isinstance(value, bool) or not isinstance(value, int) or value < minimum or value > maximum:
        raise ValueError(f"{name} must be an integer between {minimum} and {maximum}")
    return value


def _validate_http_source(raw):
    _require_keys(
        raw,
        {"kind", "url"},
        {"method", "headers", "auth", "conditional", "max_redirects"},
        "probe.source",
    )
    url = raw["url"]
    if not isinstance(url, str) or not url or url.strip() != url or len(url.encode("utf-8")) > 2048:
        raise ValueError("probe.source.url must be a non-empty trimmed URL up to 2048 bytes")
    if any(ord(ch) < 33 or ord(ch) == 127 for ch in url):
        raise ValueError("probe.source.url contains whitespace or control characters")
    parsed = urllib.parse.urlsplit(url)
    if parsed.scheme not in {"http", "https"} or not parsed.netloc:
        raise ValueError("probe.source.url must be an http:// or https:// URL with a host")
    method = raw.get("method", "GET")
    if method not in {"GET", "HEAD"}:
        raise ValueError("probe.source.method must be GET or HEAD")
    headers_raw = raw.get("headers", {})
    if not isinstance(headers_raw, dict) or len(headers_raw) > 32:
        raise ValueError("probe.source.headers must be an object with at most 32 headers")
    headers = {}
    for key, val in headers_raw.items():
        if not isinstance(key, str) or HEADER_NAME_PATTERN.fullmatch(key) is None:
            raise ValueError(f"probe.source.headers has invalid header name: {key!r}")
        if key.lower() in FORBIDDEN_STATIC_HEADERS:
            raise ValueError(f"probe.source.headers forbids '{key}'; use probe.source.auth instead")
        if not isinstance(val, str) or "\r" in val or "\n" in val or len(val.encode("utf-8")) > 2048:
            raise ValueError(f"probe.source.headers.{key} must be a single-line string up to 2048 bytes")
        headers[key] = val
    auth = None
    if "auth" in raw and raw["auth"] is not None:
        auth_raw = _require_keys(raw["auth"], {"type"}, {"header", "token_file", "env"}, "probe.source.auth")
        auth_type = auth_raw["type"]
        if auth_type not in {"bearer", "header"}:
            raise ValueError("probe.source.auth.type must be 'bearer' or 'header'")
        has_file = "token_file" in auth_raw
        has_env = "env" in auth_raw
        if has_file == has_env:
            raise ValueError("probe.source.auth must specify exactly one of token_file or env")
        auth = {"type": auth_type}
        if auth_type == "header":
            header_name = auth_raw.get("header")
            if not isinstance(header_name, str) or HEADER_NAME_PATTERN.fullmatch(header_name) is None:
                raise ValueError("probe.source.auth.header must be a valid HTTP header name")
            auth["header"] = header_name
        elif "header" in auth_raw:
            raise ValueError("probe.source.auth.header is only allowed when type is 'header'")
        if has_file:
            token_file = auth_raw["token_file"]
            if not isinstance(token_file, str) or re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._-]{0,127}", token_file) is None or token_file in {".", ".."}:
                raise ValueError("probe.source.auth.token_file must be a single filename inside OCTOPUS_VAR_DIR")
            auth["token_file"] = token_file
        if has_env:
            env_name = auth_raw["env"]
            if not isinstance(env_name, str) or ENV_NAME_PATTERN.fullmatch(env_name) is None:
                raise ValueError("probe.source.auth.env must be a valid environment variable name")
            auth["env"] = env_name
    conditional = _require_bool(raw.get("conditional", True), "probe.source.conditional")
    max_redirects = _require_int_range(raw.get("max_redirects", 3), "probe.source.max_redirects", 0, 5)
    return {
        "kind": "http",
        "url": url,
        "method": method,
        "headers": headers,
        "auth": auth,
        "conditional": conditional,
        "max_redirects": max_redirects,
    }


def _validate_file_source(raw):
    _require_keys(raw, {"kind", "path"}, {"missing_ok"}, "probe.source")
    path = raw["path"]
    if not isinstance(path, str) or not path or "\x00" in path or not os.path.isabs(path):
        raise ValueError("probe.source.path must be a non-empty absolute file path")
    missing_ok = _require_bool(raw.get("missing_ok", False), "probe.source.missing_ok")
    return {"kind": "file", "path": path, "missing_ok": missing_ok}


def _validate_webhook_source(raw):
    _require_keys(
        raw,
        {"kind", "port"},
        {"bind", "path", "methods", "auth"},
        "probe.source",
    )
    bind = raw.get("bind", "127.0.0.1")
    if not isinstance(bind, str) or not bind:
        raise ValueError("probe.source.bind must be an IP address string")
    try:
        ipaddress.ip_address(bind)
    except ValueError:
        raise ValueError("probe.source.bind must be a valid IP address") from None
    port = raw["port"]
    if isinstance(port, bool) or not isinstance(port, int) or port < 0 or port > 65535 or (0 < port < 1024):
        raise ValueError("probe.source.port must be 0 or an unprivileged integer port between 1024 and 65535")
    path = raw.get("path", "/")
    if (
        not isinstance(path, str)
        or not path.startswith("/")
        or len(path.encode("utf-8")) > 256
        or any(ch in path for ch in ("?", "#", " ", "\t", "\r", "\n"))
    ):
        raise ValueError("probe.source.path must start with '/' and contain no query, fragment, or whitespace")
    methods_raw = raw.get("methods", ["POST"])
    if (
        not isinstance(methods_raw, list)
        or not methods_raw
        or any(m not in {"POST", "PUT", "GET"} for m in methods_raw)
        or len(set(methods_raw)) != len(methods_raw)
    ):
        raise ValueError("probe.source.methods must be a non-empty list of unique methods from POST, PUT, GET")
    auth = None
    if "auth" in raw and raw["auth"] is not None:
        auth_raw = _require_keys(
            raw["auth"],
            {"type"},
            {"header", "prefix", "token_file", "env"},
            "probe.source.auth",
        )
        auth_type = auth_raw["type"]
        if auth_type not in {"bearer", "header", "hmac_sha256"}:
            raise ValueError("probe.source.auth.type must be 'bearer', 'header', or 'hmac_sha256'")
        has_file = "token_file" in auth_raw
        has_env = "env" in auth_raw
        if has_file == has_env:
            raise ValueError("probe.source.auth must specify exactly one of token_file or env")
        auth = {"type": auth_type}
        if auth_type in {"header", "hmac_sha256"}:
            header_name = auth_raw.get("header")
            if not isinstance(header_name, str) or HEADER_NAME_PATTERN.fullmatch(header_name) is None:
                raise ValueError("probe.source.auth.header must be a valid HTTP header name")
            auth["header"] = header_name
            default_prefix = "sha256=" if auth_type == "hmac_sha256" else ""
            prefix = auth_raw.get("prefix", default_prefix)
            if not isinstance(prefix, str) or len(prefix.encode("utf-8")) > 64 or "\r" in prefix or "\n" in prefix:
                raise ValueError("probe.source.auth.prefix must be a short single-line string")
            auth["prefix"] = prefix
        else:
            if "header" in auth_raw or "prefix" in auth_raw:
                raise ValueError("probe.source.auth.header/prefix are not allowed for bearer auth")
        if has_file:
            token_file = auth_raw["token_file"]
            if (
                not isinstance(token_file, str)
                or re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._-]{0,127}", token_file) is None
                or token_file in {".", ".."}
            ):
                raise ValueError("probe.source.auth.token_file must be a single filename inside OCTOPUS_VAR_DIR")
            auth["token_file"] = token_file
        if has_env:
            env_name = auth_raw["env"]
            if not isinstance(env_name, str) or ENV_NAME_PATTERN.fullmatch(env_name) is None:
                raise ValueError("probe.source.auth.env must be a valid environment variable name")
            auth["env"] = env_name
    return {
        "kind": "webhook",
        "bind": bind,
        "port": port,
        "path": path,
        "methods": list(methods_raw),
        "auth": auth,
    }


def _validate_extract(raw):
    if not isinstance(raw, dict):
        raise ValueError("probe.extract must be a JSON object")
    fmt = raw.get("format", "text")
    if fmt == "text":
        _require_keys(raw, set(), {"format", "regex", "strip"}, "probe.extract")
        regex = raw.get("regex")
        if regex is not None:
            if not isinstance(regex, str) or not regex or len(regex.encode("utf-8")) > 512:
                raise ValueError("probe.extract.regex must be a non-empty pattern up to 512 bytes")
            try:
                re.compile(regex)
            except re.error as error:
                raise ValueError(f"probe.extract.regex is invalid: {error}") from None
        strip = _require_bool(raw.get("strip", False), "probe.extract.strip")
        return {"format": "text", "regex": regex, "strip": strip}
    if fmt == "json":
        _require_keys(raw, {"format"}, {"json_pointer", "missing_as_null"}, "probe.extract")
        pointer = raw.get("json_pointer")
        if pointer is not None:
            if not isinstance(pointer, str) or (pointer != "" and not pointer.startswith("/")):
                raise ValueError("probe.extract.json_pointer must be '' or start with '/'")
            if len(pointer.encode("utf-8")) > 1024:
                raise ValueError("probe.extract.json_pointer exceeds 1024 bytes")
        missing_as_null = _require_bool(raw.get("missing_as_null", False), "probe.extract.missing_as_null")
        return {"format": "json", "json_pointer": pointer, "missing_as_null": missing_as_null}
    if fmt == "status":
        _require_keys(raw, {"format"}, set(), "probe.extract")
        return {"format": "status"}
    raise ValueError("probe.extract.format must be 'text', 'json', or 'status'")


def _validate_condition(raw):
    allowed = {
        "status_in",
        "status_not_in",
        "contains",
        "not_contains",
        "regex",
        "equals",
        "not_equals",
        "gt",
        "gte",
        "lt",
        "lte",
        "exists",
    }
    _require_keys(raw, set(), allowed, "probe.trigger.condition")
    if not raw:
        raise ValueError("probe.trigger.condition must declare at least one predicate")
    out = {}
    for key in ("status_in", "status_not_in"):
        if key in raw:
            codes = raw[key]
            if not isinstance(codes, list) or not codes or len(codes) > 64:
                raise ValueError(f"probe.trigger.condition.{key} must be a non-empty list of status codes")
            validated = [_require_int_range(code, f"probe.trigger.condition.{key}", 100, 599) for code in codes]
            out[key] = validated
    for key in ("contains", "not_contains"):
        if key in raw:
            val = raw[key]
            if not isinstance(val, str) or not val or len(val.encode("utf-8")) > 1024:
                raise ValueError(f"probe.trigger.condition.{key} must be a non-empty string up to 1024 bytes")
            out[key] = val
    if "regex" in raw:
        pattern = raw["regex"]
        if not isinstance(pattern, str) or not pattern or len(pattern.encode("utf-8")) > 512:
            raise ValueError("probe.trigger.condition.regex must be a non-empty regex up to 512 bytes")
        try:
            re.compile(pattern)
        except re.error as error:
            raise ValueError(f"probe.trigger.condition.regex is invalid: {error}") from None
        out["regex"] = pattern
    for key in ("equals", "not_equals"):
        if key in raw:
            out[key] = raw[key]
    for key in ("gt", "gte", "lt", "lte"):
        if key in raw:
            val = raw[key]
            if not _is_number(val):
                raise ValueError(f"probe.trigger.condition.{key} must be a finite number")
            out[key] = val
    if "exists" in raw:
        out["exists"] = _require_bool(raw["exists"], "probe.trigger.condition.exists")
    return out


def _validate_trigger(raw):
    if not isinstance(raw, dict):
        raise ValueError("probe.trigger must be a JSON object")
    mode = raw.get("mode")
    if mode not in {"on_change", "on_match", "always"}:
        raise ValueError("probe.trigger.mode must be 'on_change', 'on_match', or 'always'")
    if mode == "on_match":
        _require_keys(
            raw,
            {"mode", "condition"},
            {"fire_on_first", "edge_triggered", "notify_on_recovery", "cooldown_seconds", "notify_on_error"},
            "probe.trigger",
        )
        condition = _validate_condition(raw["condition"])
        edge_triggered = _require_bool(raw.get("edge_triggered", True), "probe.trigger.edge_triggered")
        notify_on_recovery = _require_bool(raw.get("notify_on_recovery", False), "probe.trigger.notify_on_recovery")
        default_first = True
    else:
        _require_keys(
            raw,
            {"mode"},
            {"fire_on_first", "cooldown_seconds", "notify_on_error"},
            "probe.trigger",
        )
        condition = None
        edge_triggered = False
        notify_on_recovery = False
        default_first = mode == "always"
    fire_on_first = _require_bool(raw.get("fire_on_first", default_first), "probe.trigger.fire_on_first")
    cooldown_seconds = _require_int_range(raw.get("cooldown_seconds", 0), "probe.trigger.cooldown_seconds", 0, 86400 * 30)
    notify_on_error = _require_bool(raw.get("notify_on_error", False), "probe.trigger.notify_on_error")
    return {
        "mode": mode,
        "condition": condition,
        "fire_on_first": fire_on_first,
        "edge_triggered": edge_triggered,
        "notify_on_recovery": notify_on_recovery,
        "cooldown_seconds": cooldown_seconds,
        "notify_on_error": notify_on_error,
    }


def validate_config(raw):
    if not isinstance(raw, dict):
        raise ValueError("probe config must be a JSON object")
    source_raw = raw.get("source")
    kind = source_raw.get("kind") if isinstance(source_raw, dict) else None
    required_top = {"enabled", "source", "trigger"} if kind == "webhook" else {"enabled", "poll_seconds", "source", "trigger"}
    optional_top = {"poll_seconds", "extract", "hold_while_working", "label", "note", "max_response_bytes", "timeout_seconds"}
    _require_keys(raw, required_top, optional_top, "probe config")
    enabled = _require_bool(raw["enabled"], "probe.enabled")
    poll_seconds = _require_int_range(
        raw.get("poll_seconds", DEFAULT_WEBHOOK_POLL_SECONDS),
        "probe.poll_seconds",
        5,
        86400,
    )
    if kind not in {"http", "file", "webhook"}:
        raise ValueError("probe.source.kind must be 'http', 'file', or 'webhook'")
    if kind == "http":
        source = _validate_http_source(source_raw)
    elif kind == "file":
        source = _validate_file_source(source_raw)
    else:
        source = _validate_webhook_source(source_raw)
    extract = _validate_extract(raw.get("extract", {"format": "text"}))
    trigger = _validate_trigger(raw["trigger"])
    hold_while_working = _require_bool(raw.get("hold_while_working", True), "probe.hold_while_working")
    label = raw.get("label", DEFAULT_LABEL)
    if not isinstance(label, str) or LABEL_PATTERN.fullmatch(label) is None:
        raise ValueError("probe.label must match ^[a-z0-9][a-z0-9._-]{0,127}$")
    note = raw.get("note", "")
    if not isinstance(note, str) or len(note.encode("utf-8")) > 2048:
        raise ValueError("probe.note must be a string up to 2048 bytes")
    max_response_bytes = _require_int_range(
        raw.get("max_response_bytes", DEFAULT_MAX_RESPONSE_BYTES),
        "probe.max_response_bytes",
        1024,
        2 * 1024 * 1024,
    )
    timeout_seconds = _require_int_range(
        raw.get("timeout_seconds", DEFAULT_TIMEOUT_SECONDS),
        "probe.timeout_seconds",
        1,
        120,
    )
    return {
        "enabled": enabled,
        "poll_seconds": poll_seconds,
        "source": source,
        "extract": extract,
        "trigger": trigger,
        "hold_while_working": hold_while_working,
        "label": label,
        "note": note,
        "max_response_bytes": max_response_bytes,
        "timeout_seconds": timeout_seconds,
    }


def resolve_json_pointer(document, pointer):
    if pointer == "":
        return True, document
    current = document
    for raw_token in pointer.split("/")[1:]:
        token = raw_token.replace("~1", "/").replace("~0", "~")
        if isinstance(current, dict):
            if token not in current:
                return False, None
            current = current[token]
        elif isinstance(current, list):
            if re.fullmatch(r"0|[1-9][0-9]*", token) is None:
                return False, None
            index = int(token)
            if index >= len(current):
                return False, None
            current = current[index]
        else:
            return False, None
    return True, current


def extract_value(extract_cfg, observation):
    if observation.get("not_modified"):
        return None
    fmt = extract_cfg["format"]
    if fmt == "status":
        return observation.get("status_code") if observation["kind"] == "http" else observation.get("exists")
    body = observation.get("body")
    if body is None:
        return None
    if fmt == "text":
        text = body.decode("utf-8", errors="replace")
        if extract_cfg.get("strip"):
            text = text.strip()
        pattern = extract_cfg.get("regex")
        if pattern:
            match = re.search(pattern, text)
            if match is None:
                return None
            return match.group(1) if match.lastindex else match.group(0)
        return text
    if fmt == "json":
        try:
            parsed = json.loads(body.decode("utf-8"))
        except (UnicodeDecodeError, json.JSONDecodeError) as error:
            raise ProbeError("invalid_json", str(error)) from None
        pointer = extract_cfg.get("json_pointer")
        if pointer is None:
            return parsed
        found, target = resolve_json_pointer(parsed, pointer)
        if not found:
            if extract_cfg.get("missing_as_null"):
                return None
            raise ProbeError("json_pointer_not_found", pointer)
        return target
    raise ProbeError("unsupported_extract_format", fmt)


def _text_representation(value):
    if value is None:
        return ""
    if isinstance(value, str):
        return value
    return canonical(value).decode("utf-8")


def evaluate_condition(condition, value, status_code, exists):
    if "status_in" in condition and status_code not in condition["status_in"]:
        return False
    if "status_not_in" in condition and (status_code is None or status_code in condition["status_not_in"]):
        return False
    if "exists" in condition:
        present = exists if exists is not None else (value is not None)
        if present is not condition["exists"]:
            return False
    if "contains" in condition and condition["contains"] not in _text_representation(value):
        return False
    if "not_contains" in condition and condition["not_contains"] in _text_representation(value):
        return False
    if "regex" in condition and re.search(condition["regex"], _text_representation(value)) is None:
        return False
    if "equals" in condition and not _strict_equal(value, condition["equals"]):
        return False
    if "not_equals" in condition and _strict_equal(value, condition["not_equals"]):
        return False
    for op in ("gt", "gte", "lt", "lte"):
        if op in condition:
            if not _is_number(value):
                return False
            bound = condition[op]
            if op == "gt" and not (value > bound):
                return False
            if op == "gte" and not (value >= bound):
                return False
            if op == "lt" and not (value < bound):
                return False
            if op == "lte" and not (value <= bound):
                return False
    return True


def _bounded_value(value):
    raw = canonical(value)
    if len(raw) <= MAX_VALUE_JSON_BYTES:
        return value, False
    preview = _text_representation(value)[:MAX_EXCERPT_CHARS]
    return preview, True


def _excerpt_for(value):
    text = _text_representation(value)
    if len(text) <= MAX_EXCERPT_CHARS:
        return text
    return text[: MAX_EXCERPT_CHARS - 1] + "…"


class _SafeRedirectHandler(urllib.request.HTTPRedirectHandler):
    def __init__(self, max_redirects, auth_header_name):
        super().__init__()
        self.max_redirects = max_redirects
        self.auth_header_name = auth_header_name

    def redirect_request(self, req, fp, code, msg, headers, newurl):
        count = getattr(req, "_octopus_redirects", 0) + 1
        if count > self.max_redirects:
            raise ProbeError("too_many_redirects", f"exceeded {self.max_redirects}")
        target = urllib.parse.urlsplit(newurl)
        if target.scheme not in {"http", "https"} or not target.netloc:
            raise ProbeError("unsafe_redirect", f"blocked scheme '{target.scheme}'")
        next_req = super().redirect_request(req, fp, code, msg, headers, newurl)
        if next_req is None:
            return None
        origin = urllib.parse.urlsplit(req.full_url)
        if self.auth_header_name and (origin.scheme, origin.netloc) != (target.scheme, target.netloc):
            for existing in list(next_req.headers):
                if existing.lower() == self.auth_header_name.lower():
                    del next_req.headers[existing]
            for existing in list(next_req.unredirected_hdrs):
                if existing.lower() == self.auth_header_name.lower():
                    del next_req.unredirected_hdrs[existing]
        next_req._octopus_redirects = count
        return next_req


class HttpProbe:
    def __init__(self, config, boundary):
        self.c = config
        self.b = boundary

    def _auth_header(self):
        auth = self.c["source"].get("auth")
        if auth is None:
            return None, None
        if "token_file" in auth:
            try:
                token = self.b.read_private_token(auth["token_file"])
            except (OSError, ValueError, UnicodeDecodeError) as error:
                raise ProbeError("auth_unavailable", str(error)) from None
        else:
            token = os.environ.get(auth["env"], "").strip()
            if not token or any(ord(ch) < 33 or ord(ch) > 126 for ch in token):
                raise ProbeError("auth_unavailable", f"environment variable {auth['env']} is missing or invalid")
        if auth["type"] == "bearer":
            return "Authorization", f"Bearer {token}"
        return auth["header"], token

    def fetch(self, prior_http):
        source = self.c["source"]
        headers = dict(source["headers"])
        headers.setdefault("User-Agent", "octopus-world-watch/1")
        auth_name, auth_value = self._auth_header()
        if auth_name:
            headers[auth_name] = auth_value
        if source["conditional"] and isinstance(prior_http, dict):
            if prior_http.get("etag"):
                headers["If-None-Match"] = prior_http["etag"]
            if prior_http.get("last_modified"):
                headers["If-Modified-Since"] = prior_http["last_modified"]
        request = urllib.request.Request(source["url"], headers=headers, method=source["method"])
        opener = urllib.request.build_opener(_SafeRedirectHandler(source["max_redirects"], auth_name))
        limit = self.c["max_response_bytes"]
        try:
            with opener.open(request, timeout=self.c["timeout_seconds"]) as response:
                body = b"" if source["method"] == "HEAD" else response.read(limit + 1)
                if len(body) > limit:
                    raise ProbeError("response_too_large", f"exceeded {limit} bytes")
                return {
                    "kind": "http",
                    "status_code": int(response.status),
                    "exists": True,
                    "not_modified": False,
                    "body": None if source["method"] == "HEAD" else body,
                    "etag": response.headers.get("ETag"),
                    "last_modified": response.headers.get("Last-Modified"),
                }
        except urllib.error.HTTPError as error:
            try:
                if error.code == 304 and source["conditional"]:
                    return {
                        "kind": "http",
                        "status_code": 304,
                        "exists": True,
                        "not_modified": True,
                        "body": None,
                        "etag": error.headers.get("ETag") if error.headers else None,
                        "last_modified": error.headers.get("Last-Modified") if error.headers else None,
                    }
                body = b"" if source["method"] == "HEAD" else error.read(limit + 1)
                if len(body) > limit:
                    body = body[:limit]
                return {
                    "kind": "http",
                    "status_code": int(error.code),
                    "exists": True,
                    "not_modified": False,
                    "body": None if source["method"] == "HEAD" else body,
                    "etag": error.headers.get("ETag") if error.headers else None,
                    "last_modified": error.headers.get("Last-Modified") if error.headers else None,
                }
            finally:
                error.close()
        except ProbeError:
            raise
        except urllib.error.URLError as error:
            raise ProbeError("network_error", str(error.reason)) from None
        except TimeoutError as error:
            raise ProbeError("timeout", str(error)) from None
        except OSError as error:
            raise ProbeError("network_error", str(error)) from None


class FileProbe:
    def __init__(self, config, boundary):
        self.c = config
        self.b = boundary

    def fetch(self, _prior_http):
        source = self.c["source"]
        path = source["path"]
        try:
            body = self.b.stable_read(path, self.c["max_response_bytes"], "watched file")
        except FileNotFoundError:
            if source["missing_ok"]:
                return {
                    "kind": "file",
                    "status_code": None,
                    "exists": False,
                    "not_modified": False,
                    "body": None,
                    "etag": None,
                    "last_modified": None,
                }
            raise ProbeError("file_missing", path) from None
        except (OSError, ValueError) as error:
            raise ProbeError("file_read_error", str(error)) from None
        return {
            "kind": "file",
            "status_code": None,
            "exists": True,
            "not_modified": False,
            "body": body,
            "etag": None,
            "last_modified": None,
        }


def build_probe(config, boundary):
    kind = config["source"]["kind"]
    if kind == "http":
        return HttpProbe(config, boundary)
    if kind == "file":
        return FileProbe(config, boundary)
    raise ValueError("webhook source uses WebhookServer, not a polling probe")


def _source_identity(config):
    source = config["source"]
    if source["kind"] == "http":
        return {"kind": "http", "url": source["url"], "method": source["method"]}
    if source["kind"] == "file":
        return {"kind": "file", "path": source["path"]}
    return {"kind": "webhook", "bind": source["bind"], "port": source["port"], "path": source["path"]}


def _public_source_descriptor(config):
    source = config["source"]
    if source["kind"] == "http":
        return {"kind": "http", "url": source["url"], "method": source["method"]}
    if source["kind"] == "file":
        return {"kind": "file", "path": source["path"]}
    return {"kind": "webhook", "bind": source["bind"], "port": source["port"], "path": source["path"]}


def _load_webhook_secret(auth, boundary):
    if "token_file" in auth:
        try:
            return boundary.read_private_token(auth["token_file"])
        except (OSError, ValueError, UnicodeDecodeError) as error:
            raise ProbeError("auth_unavailable", str(error)) from None
    token = os.environ.get(auth["env"], "").strip()
    if not token or any(ord(ch) < 33 or ord(ch) > 126 for ch in token):
        raise ProbeError("auth_unavailable", f"environment variable {auth['env']} is missing or invalid")
    return token


def verify_webhook_auth(auth, headers, raw_body, boundary):
    if auth is None:
        return True
    secret = _load_webhook_secret(auth, boundary)
    auth_type = auth["type"]
    if auth_type == "bearer":
        supplied = headers.get("Authorization") or ""
        return hmac.compare_digest(supplied, f"Bearer {secret}")
    if auth_type == "header":
        supplied = headers.get(auth["header"]) or ""
        return hmac.compare_digest(supplied, f"{auth['prefix']}{secret}")
    if auth_type == "hmac_sha256":
        supplied = headers.get(auth["header"]) or ""
        prefix = auth["prefix"]
        if not supplied.startswith(prefix):
            return False
        supplied_hex = supplied[len(prefix):].lower()
        expected_hex = hmac.new(secret.encode("utf-8"), raw_body, hashlib.sha256).hexdigest()
        return hmac.compare_digest(supplied_hex, expected_hex)
    return False


class _WebhookThreadingServer(http.server.ThreadingHTTPServer):
    allow_reuse_address = True
    daemon_threads = True
    request_queue_size = 16


class WebhookServer:
    def __init__(self, config, watcher, boundary, wake_event=None):
        self.c = config
        self.watcher = watcher
        self.b = boundary
        self.wake_event = wake_event
        source = config["source"]
        bind_ip = ipaddress.ip_address(source["bind"])
        server_cls = type(
            "_BoundWebhookServer",
            (_WebhookThreadingServer,),
            {"address_family": socket.AF_INET6 if bind_ip.version == 6 else socket.AF_INET},
        )
        owner = self

        class Handler(http.server.BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"

            def setup(self):
                super().setup()
                self.connection.settimeout(owner.c["timeout_seconds"])

            def log_message(self, *_args):
                return

            def _send_json(self, code, payload):
                raw = canonical(payload)
                self.send_response(code)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(raw)))
                self.send_header("Connection", "close")
                self.end_headers()
                self.wfile.write(raw)

            def _handle(self, method):
                src = owner.c["source"]
                req_path = urllib.parse.urlsplit(self.path).path
                if req_path != src["path"]:
                    self._send_json(404, {"accepted": False, "error": "not_found"})
                    return
                if method not in src["methods"]:
                    self._send_json(405, {"accepted": False, "error": "method_not_allowed"})
                    return
                limit = owner.c["max_response_bytes"]
                if method == "GET":
                    raw_body = b""
                else:
                    length_hdr = self.headers.get("Content-Length")
                    if length_hdr is None:
                        self._send_json(411, {"accepted": False, "error": "length_required"})
                        return
                    try:
                        length = int(length_hdr)
                    except ValueError:
                        self._send_json(400, {"accepted": False, "error": "invalid_content_length"})
                        return
                    if length < 0:
                        self._send_json(400, {"accepted": False, "error": "invalid_content_length"})
                        return
                    if length > limit:
                        self._send_json(413, {"accepted": False, "error": "payload_too_large"})
                        return
                    raw_body = self.rfile.read(length) if length > 0 else b""
                    if len(raw_body) != length:
                        self._send_json(400, {"accepted": False, "error": "truncated_body"})
                        return
                try:
                    if not verify_webhook_auth(src["auth"], self.headers, raw_body, owner.b):
                        self._send_json(401, {"accepted": False, "error": "unauthorized"})
                        return
                except ProbeError as error:
                    self._send_json(503, {"accepted": False, "error": error.code})
                    return
                observation = {
                    "kind": "webhook",
                    "status_code": None,
                    "method": method,
                    "path": req_path,
                    "exists": True,
                    "not_modified": False,
                    "body": raw_body,
                    "etag": None,
                    "last_modified": None,
                }
                try:
                    staged, reason = owner.watcher.ingest_push(observation)
                except ProbeError as error:
                    status = 429 if error.code == "queue_full" else 400
                    self._send_json(status, {"accepted": False, "error": error.code})
                    return
                if staged:
                    if owner.wake_event is not None:
                        owner.wake_event.set()
                    self._send_json(202, {"accepted": True, "triggered": True, "reason": reason})
                else:
                    self._send_json(200, {"accepted": True, "triggered": False})

            def do_POST(self):
                self._handle("POST")

            def do_PUT(self):
                self._handle("PUT")

            def do_GET(self):
                self._handle("GET")

        self.server = server_cls((source["bind"], source["port"]), Handler)
        self.thread = None

    @property
    def port(self):
        return self.server.server_address[1]

    def start(self):
        self.thread = threading.Thread(target=self.server.serve_forever, kwargs={"poll_interval": 0.25}, daemon=True)
        self.thread.start()

    def stop(self):
        self.server.shutdown()
        self.server.server_close()
        if self.thread is not None:
            self.thread.join(timeout=5.0)
            self.thread = None


class Watcher:
    def __init__(self, config, boundary):
        self.c = config
        self.b = boundary
        self.lock = threading.RLock()
        boundary.private_directory(boundary.VAR_DIR)
        boundary.private_directory(boundary.PAYLOADS_DIR)
        self.path = Path(boundary.VAR_DIR) / "watch.sqlite3"
        if os.path.lexists(self.path):
            boundary.stable_read(self.path, 128 * 1024 * 1024, "watch database", private=True)
        self.db = sqlite3.connect(str(self.path), check_same_thread=False)
        os.chmod(self.path, 0o600)
        self.db.execute("PRAGMA journal_mode=DELETE")
        self.db.execute("PRAGMA synchronous=FULL")
        self.db.execute("CREATE TABLE IF NOT EXISTS records (name TEXT PRIMARY KEY, value TEXT NOT NULL)")
        self.db.commit()
        self.health = self.read("health") or {"last_success_at": None, "failures": 0}

    def close(self):
        with self.lock:
            self.db.close()

    def reconfigure(self, config):
        with self.lock:
            self.c = config

    def read(self, name):
        with self.lock:
            row = self.db.execute("SELECT value FROM records WHERE name=?", (name,)).fetchone()
            return json.loads(row[0]) if row else None

    def write(self, name, value):
        with self.lock:
            raw = canonical(value)
            if len(raw) > MAX_STATE_BYTES:
                raise ProbeError("state_byte_limit", "watch state exceeds limit")
            self.db.execute("INSERT OR REPLACE INTO records VALUES (?,?)", (name, raw.decode("utf-8")))

    def status(self, state, error=None, **extra):
        with self.lock:
            self.health.update(state=state, error=error, updated_at=now_iso(), **extra)
            with self.db:
                self.write("health", self.health)
            self.b.atomic_json(str(Path(self.b.VAR_DIR) / "health.json"), self.health)

    def _base_state(self, observed_at):
        identity = _source_identity(self.c)
        pending = self.read("pending")
        if (
            pending is not None
            and isinstance(pending.get("candidate"), dict)
            and pending["candidate"].get("schema_version") == 1
            and pending["candidate"].get("identity") == identity
        ):
            return json.loads(json.dumps(pending["candidate"])), False
        previous = self.read("state")
        if previous is not None and previous.get("schema_version") == 1 and previous.get("identity") == identity:
            return json.loads(json.dumps(previous)), False
        fresh = {
            "schema_version": 1,
            "identity": identity,
            "seeded_at": observed_at,
            "generation": 0,
            "last_digest": None,
            "last_value": None,
            "last_value_truncated": False,
            "last_status_code": None,
            "last_exists": None,
            "last_matched": None,
            "last_fired_at_unix": None,
            "in_error": False,
            "http": {},
        }
        return fresh, True

    def _cooldown_ready(self, state, now_unix):
        cooldown = self.c["trigger"]["cooldown_seconds"]
        last_fired = state.get("last_fired_at_unix")
        if cooldown <= 0 or last_fired is None:
            return True
        return (now_unix - last_fired) >= cooldown

    def _stage_notice(self, candidate, payload_body):
        with self.lock:
            existing = self.read("pending")
            notices = list(existing["notices"]) if existing else []
            if len(notices) >= MAX_PENDING_NOTICES:
                raise ProbeError("queue_full", f"pending queue reached {MAX_PENDING_NOTICES}")
            payload = canonical(payload_body)
            digest = self.b.octopus_content_hash(payload)
            payload_path = str(Path(self.b.PAYLOADS_DIR) / (digest + ".payload"))
            self.b.private_file(payload_path, payload, self.b.IO_LIMITS["max_payload_bytes"], "watch notice")
            label = self.c["label"]
            manifest = canonical({"label": label, "payload_path": payload_path, "payload_content_hash": digest})
            manifest_hash = self.b.octopus_content_hash(manifest)
            manifest_path = str(Path(self.b.PAYLOADS_DIR) / (manifest_hash + ".artifact.json"))
            self.b.private_file(manifest_path, manifest, self.b.IO_LIMITS["max_manifest_bytes"], "watch manifest")
            notice = {
                "label": label,
                "manifest_path": manifest_path,
                "manifest_hash": manifest_hash,
                "admitted_seq": None,
            }
            notices.append(notice)
            with self.db:
                self.write("pending", {"candidate": candidate, "notices": notices})
            return notice

    def ingest_push(self, observation, now_unix=None):
        now_unix = time.time() if now_unix is None else now_unix
        observed_at = now_iso()
        with self.lock:
            try:
                staged, reason = self._prepare_observation(observation, observed_at, now_unix)
            except ProbeError as error:
                if error.code != "queue_full":
                    self._prepare_error(error, observed_at, now_unix)
                    self.status("probe_failed", error=error.code, error_detail=error.detail, pending=self.read("pending") is not None)
                raise
            self.status(
                "listening",
                error=None,
                error_detail=None,
                failures=0,
                last_success_at=observed_at,
                last_reason=reason,
                pending=self.read("pending") is not None,
            )
            return staged, reason

    def deliver(self):
        last_admitted = None
        while True:
            with self.lock:
                pending = self.read("pending")
                if pending is None:
                    return "delivered" if last_admitted is not None else "clear"
                if self.c["hold_while_working"]:
                    working = self.b.working_pinning_runs()
                    if working:
                        self.status("holding", error=None, held_by=working, pending=True)
                        return "held"
                target_index = None
                target_notice = None
                for idx, notice in enumerate(pending["notices"]):
                    self.b.verify_prepared(notice)
                    artifact_id = notice["label"] + ":" + notice["manifest_hash"]
                    admitted = self.b.find_admitted_arrival_seq(artifact_id)
                    if notice["admitted_seq"] is not None and admitted != notice["admitted_seq"]:
                        raise RuntimeError("recorded admission missing from ledger")
                    if admitted is not None:
                        if notice["admitted_seq"] is None:
                            notice["admitted_seq"] = admitted
                            with self.db:
                                self.write("pending", pending)
                        last_admitted = admitted
                        continue
                    target_index = idx
                    target_notice = dict(notice)
                    break
                if target_notice is None:
                    completed = list(pending["notices"])
                    with self.db:
                        self.write("state", pending["candidate"])
                        self.db.execute("DELETE FROM records WHERE name='pending'")
                    self.reclaim_completed(completed)
                    idle_state = "listening" if self.c["source"]["kind"] == "webhook" else "watching"
                    self.status(idle_state, pending=False, held_by=[], last_admitted_seq=last_admitted, refusal_reason=None)
                    return "delivered"

            artifact_id = target_notice["label"] + ":" + target_notice["manifest_hash"]
            evidence = [
                {
                    "interface_id": self.b.INTERFACE_ID,
                    "artifact_id": artifact_id,
                    "label": "world.watch.idempotency",
                    "bytes": list(bytes.fromhex(self.b.octopus_content_hash(artifact_id.encode("utf-8")))),
                }
            ]
            try:
                admitted = self.b.hand(target_notice["manifest_path"], evidence)
            except self.b.Refused as refusal:
                if refusal.duplicate_of(artifact_id):
                    admitted = self.b.find_admitted_arrival_seq(artifact_id)
                    if admitted is None:
                        raise RuntimeError("duplicate verdict without ledger admission") from None
                else:
                    self.status(
                        "delivery_refused",
                        error="runtime_refused",
                        refusal_reason=refusal.reason,
                        pending=True,
                    )
                    return "refused"
            except Exception:
                self.status("delivery_uncertain", error="runtime_verdict_uncertain", pending=True)
                raise

            with self.lock:
                pending = self.read("pending")
                if pending is not None and target_index < len(pending["notices"]):
                    pending["notices"][target_index]["admitted_seq"] = admitted
                    with self.db:
                        self.write("pending", pending)
                last_admitted = admitted

    def reclaim_completed(self, notices):
        if self.db.in_transaction:
            raise RuntimeError("staging cleanup requires a committed database")
        pending = self.read("pending")
        protected = [item["manifest_hash"] for item in pending["notices"]] if pending else []
        completed = set()
        for notice in notices:
            sequence = notice["admitted_seq"]
            artifact_id = notice["label"] + ":" + notice["manifest_hash"]
            if type(sequence) is not int or self.b.find_admitted_arrival_seq(artifact_id, sequence) != sequence:
                raise RuntimeError("staging cleanup requires confirmed admission")
            completed.add(notice["manifest_hash"])
        self.b.reclaim_staging_pairs(completed, protected)

    def _prepare_observation(self, observation, observed_at, now_unix):
        candidate, is_first = self._base_state(observed_at)
        previous_digest = candidate["last_digest"]
        previous_value = candidate["last_value"]
        previous_matched = candidate["last_matched"]
        was_in_error = bool(candidate.get("in_error"))
        candidate["generation"] = candidate.get("generation", 0) + 1
        candidate["last_poll_at"] = observed_at
        candidate["in_error"] = False
        if observation["kind"] == "http":
            http_cache = dict(candidate.get("http") or {})
            if observation.get("etag"):
                http_cache["etag"] = observation["etag"]
            if observation.get("last_modified"):
                http_cache["last_modified"] = observation["last_modified"]
            candidate["http"] = http_cache

        if observation.get("not_modified") and not is_first:
            raw_value = previous_value
            value = previous_value
            value_truncated = bool(candidate.get("last_value_truncated"))
            digest = previous_digest
            status_code = candidate.get("last_status_code")
            exists = candidate.get("last_exists")
            changed = False
        else:
            raw_value = extract_value(self.c["extract"], observation)
            digest = self.b.octopus_content_hash(canonical(raw_value))
            value, value_truncated = _bounded_value(raw_value)
            status_code = observation.get("status_code")
            exists = observation.get("exists")
            changed = previous_digest is not None and digest != previous_digest

        trigger = self.c["trigger"]
        mode = trigger["mode"]
        cooldown_ready = self._cooldown_ready(candidate, now_unix)
        reason = None
        matched = None

        if mode == "on_change":
            if is_first:
                if trigger["fire_on_first"]:
                    reason = "initial"
            elif changed and cooldown_ready:
                reason = "changed"
            if is_first or not changed or reason is not None:
                candidate["last_digest"] = digest
                candidate["last_value"] = value
                candidate["last_value_truncated"] = value_truncated
                candidate["last_status_code"] = status_code
                candidate["last_exists"] = exists
        elif mode == "on_match":
            if observation.get("not_modified") and not is_first and previous_matched is not None:
                matched = previous_matched
            else:
                matched = evaluate_condition(trigger["condition"], raw_value, status_code, exists)
            if is_first:
                if matched and trigger["fire_on_first"]:
                    reason = "matched"
            elif trigger["edge_triggered"]:
                if not previous_matched and matched and cooldown_ready:
                    reason = "matched"
                elif previous_matched and not matched and trigger["notify_on_recovery"] and cooldown_ready:
                    reason = "recovered"
            else:
                if matched and cooldown_ready:
                    reason = "matched"
                elif previous_matched and not matched and trigger["notify_on_recovery"] and cooldown_ready:
                    reason = "recovered"
            candidate["last_digest"] = digest
            candidate["last_value"] = value
            candidate["last_value_truncated"] = value_truncated
            candidate["last_status_code"] = status_code
            candidate["last_exists"] = exists
            if reason is not None or not (
                (not previous_matched and matched and not cooldown_ready)
                or (previous_matched and not matched and trigger["notify_on_recovery"] and not cooldown_ready)
            ):
                candidate["last_matched"] = matched
        else:
            if (is_first and trigger["fire_on_first"]) or (not is_first and cooldown_ready):
                reason = "push" if observation["kind"] == "webhook" else "periodic"
            candidate["last_digest"] = digest
            candidate["last_value"] = value
            candidate["last_value_truncated"] = value_truncated
            candidate["last_status_code"] = status_code
            candidate["last_exists"] = exists

        if reason is None and was_in_error and trigger["notify_on_error"] and cooldown_ready:
            reason = "error_recovered"

        if reason is None:
            with self.db:
                self.write("state", candidate)
            return False, None

        candidate["last_fired_at_unix"] = now_unix
        payload = {
            "schema_version": 1,
            "interface_id": self.b.INTERFACE_ID,
            "observed_at": observed_at,
            "generation": candidate["generation"],
            "reason": reason,
            "source": _public_source_descriptor(self.c),
            "status_code": status_code,
            "exists": exists,
            "digest": digest,
            "previous_digest": previous_digest,
            "value": value,
            "value_truncated": value_truncated,
            "previous_value": previous_value,
            "excerpt": _excerpt_for(value),
            "note": self.c["note"],
            "untrusted_external_data": True,
        }
        if matched is not None:
            payload["matched"] = matched
        self._stage_notice(candidate, payload)
        return True, reason

    def _prepare_error(self, error, observed_at, now_unix):
        candidate, _ = self._base_state(observed_at)
        was_in_error = bool(candidate.get("in_error"))
        candidate["generation"] = candidate.get("generation", 0) + 1
        candidate["last_poll_at"] = observed_at
        candidate["in_error"] = True
        if not self.c["trigger"]["notify_on_error"] or was_in_error or not self._cooldown_ready(candidate, now_unix):
            with self.db:
                self.write("state", candidate)
            return False
        candidate["last_fired_at_unix"] = now_unix
        payload = {
            "schema_version": 1,
            "interface_id": self.b.INTERFACE_ID,
            "observed_at": observed_at,
            "generation": candidate["generation"],
            "reason": "probe_error",
            "source": _public_source_descriptor(self.c),
            "error_code": error.code,
            "error_detail": error.detail,
            "note": self.c["note"],
            "untrusted_external_data": True,
        }
        self._stage_notice(candidate, payload)
        return True

    def cycle(self, probe, now_unix=None):
        now_unix = time.time() if now_unix is None else now_unix
        delivery = self.deliver()
        if delivery == "held":
            return HOLD_RETRY_SECONDS
        if delivery == "refused":
            return self.c["poll_seconds"]
        observed_at = now_iso()
        prior = self.read("state") or {}
        try:
            observation = probe.fetch(prior.get("http"))
            staged, reason = self._prepare_observation(observation, observed_at, now_unix)
        except ProbeError as error:
            staged_err = self._prepare_error(error, observed_at, now_unix)
            retry_at = max(now_unix + self.c["poll_seconds"], error.retry_at or 0)
            self.status(
                "probe_failed",
                error=error.code,
                error_detail=error.detail,
                failures=self.health.get("failures", 0) + 1,
                pending=staged_err,
                next_poll_at=retry_at,
            )
            if staged_err:
                res = self.deliver()
                if res == "held":
                    return HOLD_RETRY_SECONDS
            return max(1.0, retry_at - now_unix)
        self.status(
            "watching",
            error=None,
            error_detail=None,
            failures=0,
            last_success_at=observed_at,
            last_reason=reason,
            pending=staged,
            next_poll_at=now_unix + self.c["poll_seconds"],
        )
        if staged:
            res = self.deliver()
            if res == "held":
                return HOLD_RETRY_SECONDS
        return self.c["poll_seconds"]
