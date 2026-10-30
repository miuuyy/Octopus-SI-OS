import base64
import hashlib
import json
import os
import re

from octopus_content_hash import octopus_content_hash


CHANNEL_RE = re.compile(r"^[CDG][A-Z0-9]{8,32}$")
TEAM_RE = re.compile(r"^T[A-Z0-9]{8,32}$")
USER_RE = re.compile(r"^[UW][A-Z0-9]{8,32}$")
TS_RE = re.compile(r"^[0-9]{1,20}\.[0-9]{1,12}$")


def require_object(value, name):
    if not isinstance(value, dict):
        raise ValueError(f"{name} must be a JSON object")
    return value


def require_keys(value, required, optional, name):
    value = require_object(value, name)
    missing = sorted(set(required) - set(value))
    unknown = sorted(set(value) - set(required) - set(optional))
    if missing:
        raise ValueError(f"{name} is missing keys: {', '.join(missing)}")
    if unknown:
        raise ValueError(f"{name} has unknown keys: {', '.join(unknown)}")
    return value


def require_string(value, name, allow_empty=False, maximum=None):
    if not isinstance(value, str) or value.strip() != value or (not allow_empty and not value):
        raise ValueError(f"{name} must be a{' possibly empty' if allow_empty else ' non-empty'} trimmed string")
    if maximum is not None and len(value) > maximum:
        raise ValueError(f"{name} exceeds {maximum} characters")
    return value


def require_bool(value, name):
    if not isinstance(value, bool):
        raise ValueError(f"{name} must be a boolean")
    return value


def require_int(value, name, minimum=None, maximum=None):
    if isinstance(value, bool) or not isinstance(value, int):
        raise ValueError(f"{name} must be an integer")
    if minimum is not None and value < minimum:
        raise ValueError(f"{name} must be at least {minimum}")
    if maximum is not None and value > maximum:
        raise ValueError(f"{name} must be at most {maximum}")
    return value


def require_string_list(value, name, allow_empty=False):
    if not isinstance(value, list) or (not allow_empty and not value):
        raise ValueError(f"{name} must be an array")
    result = []
    seen = set()
    for index, item in enumerate(value):
        item = require_string(item, f"{name}[{index}]")
        if item in seen:
            raise ValueError(f"{name} repeats {item!r}")
        seen.add(item)
        result.append(item)
    return result


def _validate_id_list(values, name, pattern):
    for value in require_string_list(values, name, True):
        if not pattern.fullmatch(value):
            raise ValueError(f"{name} contains malformed Slack id {value!r}")


ENVIRONMENT_KEYS = ("bot_token_env", "app_token_env", "allowed_user_ids_env", "allowed_channel_ids_env", "home_channel_env")


def validate_environment(value):
    environment = require_keys(value, set(ENVIRONMENT_KEYS), set(), "Slack interface environment")
    names = [require_string(environment[key], f"Slack interface environment.{key}") for key in ENVIRONMENT_KEYS]
    if len(set(names)) != len(names):
        raise ValueError("Slack interface environment names must be distinct")
    return environment


def validate_config(config):
    require_keys(
        config,
        {"schema_version", "ledger", "transport", "authz", "gating", "intake", "media", "outbound", "environment"},
        set(),
        "Slack config",
    )
    if config["schema_version"] != 2:
        raise ValueError("Slack config schema_version must be 2")
    validate_environment(config["environment"])
    ledger = require_keys(config["ledger"], {"max_row_bytes"}, set(), "Slack ledger")
    require_int(ledger["max_row_bytes"], "Slack ledger.max_row_bytes", 256, 16777216)
    transport = require_keys(
        config["transport"],
        {"api_base_env", "api_base_default", "socket_url_env", "proxy_env", "connect_timeout_ms", "request_timeout_ms", "max_response_bytes", "max_frame_bytes", "reconnect_backoff_ms", "reconnect_backoff_cap_ms", "rate_limit_max_wait_ms", "read_retry_limit"},
        set(),
        "Slack transport",
    )
    for key in ("api_base_env", "api_base_default", "socket_url_env", "proxy_env"):
        require_string(transport[key], f"Slack transport.{key}")
    for key, low, high in (
        ("connect_timeout_ms", 100, 120000),
        ("request_timeout_ms", 100, 600000),
        ("max_response_bytes", 1024, 67108864),
        ("max_frame_bytes", 1024, 134217728),
        ("reconnect_backoff_ms", 10, 60000),
        ("reconnect_backoff_cap_ms", 10, 600000),
        ("rate_limit_max_wait_ms", 0, 600000),
        ("read_retry_limit", 0, 10),
    ):
        require_int(transport[key], f"Slack transport.{key}", low, high)
    if transport["reconnect_backoff_cap_ms"] < transport["reconnect_backoff_ms"]:
        raise ValueError("Slack reconnect cap must not be below its initial backoff")
    authz = require_keys(
        config["authz"],
        {"allow_all_users", "allow_all_users_env", "allowed_user_ids", "allowed_channel_ids", "allowed_team_ids", "allowed_team_ids_env", "deny_unmatched"},
        set(),
        "Slack authz",
    )
    require_bool(authz["allow_all_users"], "Slack authz.allow_all_users")
    require_bool(authz["deny_unmatched"], "Slack authz.deny_unmatched")
    for key in ("allow_all_users_env", "allowed_team_ids_env"):
        require_string(authz[key], f"Slack authz.{key}")
    _validate_id_list(authz["allowed_user_ids"], "Slack authz.allowed_user_ids", USER_RE)
    _validate_id_list(authz["allowed_channel_ids"], "Slack authz.allowed_channel_ids", CHANNEL_RE)
    _validate_id_list(authz["allowed_team_ids"], "Slack authz.allowed_team_ids", TEAM_RE)
    gating = require_keys(
        config["gating"],
        {"respond_in_dms", "respond_in_channels_on_mention_only", "free_response_channel_ids", "free_response_channel_ids_env", "mention_patterns", "thread_sticky_after_mention", "thread_sticky_after_bot_reply", "accept_message_edits", "accept_message_deletes", "accept_bot_events"},
        set(),
        "Slack gating",
    )
    for key in ("respond_in_dms", "respond_in_channels_on_mention_only", "thread_sticky_after_mention", "thread_sticky_after_bot_reply", "accept_message_edits", "accept_message_deletes", "accept_bot_events"):
        require_bool(gating[key], f"Slack gating.{key}")
    _validate_id_list(gating["free_response_channel_ids"], "Slack gating.free_response_channel_ids", CHANNEL_RE)
    require_string(gating["free_response_channel_ids_env"], "Slack gating.free_response_channel_ids_env")
    for index, pattern in enumerate(require_string_list(gating["mention_patterns"], "Slack gating.mention_patterns", True)):
        try:
            re.compile(pattern, re.IGNORECASE)
        except re.error as error:
            raise ValueError(f"Slack gating.mention_patterns[{index}] is invalid: {error}") from error
    intake = require_keys(config["intake"], {"event_label", "event_types", "interactive_types", "max_pending_events", "dedup_retention", "thread_context_max_messages"}, set(), "Slack intake")
    require_string(intake["event_label"], "Slack intake.event_label")
    require_string_list(intake["event_types"], "Slack intake.event_types")
    require_string_list(intake["interactive_types"], "Slack intake.interactive_types")
    require_int(intake["max_pending_events"], "Slack intake.max_pending_events", 1, 1000000)
    require_int(intake["dedup_retention"], "Slack intake.dedup_retention", 1, 1000000)
    require_int(intake["thread_context_max_messages"], "Slack intake.thread_context_max_messages", 0, 1000)
    media = require_keys(config["media"], {"download_enabled", "max_download_bytes", "max_upload_bytes", "max_files_per_event", "text_preview_bytes", "allowed_download_hosts", "allowed_upload_hosts", "retention_days", "file_label", "max_images_per_event", "max_image_bytes_per_event"}, set(), "Slack media")
    require_bool(media["download_enabled"], "Slack media.download_enabled")
    require_int(media["retention_days"], "Slack media.retention_days", 1, 3650)
    require_string(media["file_label"], "Slack media.file_label")
    require_int(media["max_images_per_event"], "Slack media.max_images_per_event", 0, 16)
    require_int(media["max_image_bytes_per_event"], "Slack media.max_image_bytes_per_event", 0, 67108864)
    require_int(media["max_download_bytes"], "Slack media.max_download_bytes", 1, 1073741824)
    require_int(media["max_upload_bytes"], "Slack media.max_upload_bytes", 1, 5368709120)
    require_int(media["max_files_per_event"], "Slack media.max_files_per_event", 0, 100)
    require_int(media["text_preview_bytes"], "Slack media.text_preview_bytes", 0, 10485760)
    for key in ("allowed_download_hosts", "allowed_upload_hosts"):
        for host in require_string_list(media[key], f"Slack media.{key}"):
            if host.lower() != host or "." not in host or "/" in host:
                raise ValueError(f"Slack media.{key} contains invalid host {host!r}")
    outbound = require_keys(config["outbound"], {"max_message_chars", "max_manifest_bytes", "max_payload_bytes", "max_blocks", "max_block_json_bytes"}, set(), "Slack outbound")
    require_int(outbound["max_message_chars"], "Slack outbound.max_message_chars", 1, 40000)
    require_int(outbound["max_manifest_bytes"], "Slack outbound.max_manifest_bytes", 1, 16777216)
    require_int(outbound["max_payload_bytes"], "Slack outbound.max_payload_bytes", 1, 1073741824)
    require_int(outbound["max_blocks"], "Slack outbound.max_blocks", 0, 50)
    require_int(outbound["max_block_json_bytes"], "Slack outbound.max_block_json_bytes", 2, 1048576)
    return config


def env_flag(name):
    value = os.environ.get(name)
    if value is None:
        return None
    lowered = value.strip().lower()
    if lowered in {"1", "true", "yes", "on"}:
        return True
    if lowered in {"0", "false", "no", "off"}:
        return False
    raise ValueError(f"{name} must be a boolean")


def env_ids(name, pattern):
    raw = os.environ.get(name, "")
    values = {part.strip() for part in raw.split(",") if part.strip()}
    for value in values:
        if not pattern.fullmatch(value):
            raise ValueError(f"{name} contains malformed Slack id {value!r}")
    return values


def rich_text(blocks):
    if not isinstance(blocks, list):
        return ""
    lines = []

    def inline(elements):
        pieces = []
        for element in elements if isinstance(elements, list) else []:
            if not isinstance(element, dict):
                continue
            kind = element.get("type")
            if kind == "text":
                pieces.append(str(element.get("text") or ""))
            elif kind == "link":
                url = str(element.get("url") or "")
                pieces.append(str(element.get("text") or url))
                if element.get("text") and url:
                    pieces.append(f" ({url})")
            elif kind == "channel":
                pieces.append(f"<#{element.get('channel_id', '')}>")
            elif kind == "user":
                pieces.append(f"<@{element.get('user_id', '')}>")
            elif kind == "usergroup":
                pieces.append(f"<!subteam^{element.get('usergroup_id', '')}>")
            elif kind == "emoji":
                pieces.append(f":{element.get('name', '')}:")
            elif kind == "broadcast":
                pieces.append(f"<!{element.get('range', 'here')}>")
            elif kind == "date":
                pieces.append(str(element.get("fallback") or ""))
        return "".join(pieces)

    def walk(elements, quote=0, bullet=""):
        for element in elements if isinstance(elements, list) else []:
            if not isinstance(element, dict):
                continue
            kind = element.get("type")
            if kind == "rich_text_section":
                text = inline(element.get("elements"))
                if text.strip():
                    lines.append((">" * quote + " " if quote else "") + bullet + text)
            elif kind == "rich_text_quote":
                walk(element.get("elements"), quote + 1, bullet)
            elif kind == "rich_text_list":
                for index, item in enumerate(element.get("elements") or []):
                    marker = "• " if element.get("style") == "bullet" else f"{index + 1}. "
                    walk([item], quote, marker)
            elif kind == "rich_text_preformatted":
                text = inline(element.get("elements"))
                if text:
                    lines.append((">" * quote + " " if quote else "") + bullet + f"```\n{text}\n```")
    for block in blocks:
        if isinstance(block, dict) and block.get("type") == "rich_text":
            walk(block.get("elements"))
    return "\n".join(lines)


def safe_blocks(blocks, maximum_bytes):
    if blocks is None:
        return []
    if not isinstance(blocks, list):
        raise ValueError("Slack blocks must be an array")
    encoded = json.dumps(blocks, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
    if len(encoded) > maximum_bytes:
        raise ValueError("Slack blocks exceed configured byte ceiling")
    return blocks


def slack_mrkdwn(text):
    text = require_string(text, "Slack message text", allow_empty=True)
    parts = re.split(r"(```[\s\S]*?```|`[^`\n]*`)", text)
    for index in range(0, len(parts), 2):
        value = parts[index]
        value = re.sub(r"\[([^\]\n]+)\]\((https?://[^\s)]+)\)", r"<\2|\1>", value)
        value = re.sub(r"(?m)^#{1,6}\s+(.+)$", r"*\1*", value)
        value = re.sub(r"\*\*([^*\n]+)\*\*", r"*\1*", value)
        value = re.sub(r"__([^_\n]+)__", r"*\1*", value)
        value = re.sub(r"~~([^~\n]+)~~", r"~\1~", value)
        parts[index] = value
    return "".join(parts)


def event_coordinates(envelope):
    payload = envelope.get("payload") if isinstance(envelope.get("payload"), dict) else {}
    outer_type = envelope.get("type")
    if outer_type == "events_api":
        event = payload.get("event") if isinstance(payload.get("event"), dict) else {}
        team = payload.get("team_id") or event.get("team") or event.get("team_id")
        item = event.get("item") if isinstance(event.get("item"), dict) else {}
        assistant_thread = event.get("assistant_thread") if isinstance(event.get("assistant_thread"), dict) else {}
        context = assistant_thread.get("context") if isinstance(assistant_thread.get("context"), dict) else {}
        channel = event.get("channel") or event.get("channel_id") or item.get("channel") or assistant_thread.get("channel_id") or context.get("channel_id")
        thread = event.get("thread_ts")
        thread = thread or assistant_thread.get("thread_ts")
        ts = event.get("ts") or event.get("event_ts") or item.get("ts")
        if event.get("subtype") in {"message_changed", "message_deleted"}:
            message = event.get("message") if isinstance(event.get("message"), dict) else {}
            previous = event.get("previous_message") if isinstance(event.get("previous_message"), dict) else {}
            thread = message.get("thread_ts") or previous.get("thread_ts") or thread
            ts = message.get("ts") or previous.get("ts") or ts
        return team, channel, thread, ts
    if outer_type == "slash_commands":
        return payload.get("team_id"), payload.get("channel_id"), None, None
    if outer_type == "interactive":
        team = (payload.get("team") or {}).get("id") if isinstance(payload.get("team"), dict) else payload.get("team_id")
        container = payload.get("container") if isinstance(payload.get("container"), dict) else {}
        channel = container.get("channel_id") or ((payload.get("channel") or {}).get("id") if isinstance(payload.get("channel"), dict) else None)
        thread = container.get("thread_ts")
        ts = container.get("message_ts")
        return team, channel, thread, ts
    return None, None, None, None


def envelope_key(envelope):
    payload = envelope.get("payload")
    if envelope.get("type") == "events_api" and isinstance(payload, dict):
        event_id = payload.get("event_id")
        if isinstance(event_id, str) and event_id:
            return "event-" + hashlib.sha256(event_id.encode("utf-8")).hexdigest()
    key = envelope.get("envelope_id")
    if isinstance(key, str) and key:
        return "envelope-" + hashlib.sha256(key.encode("utf-8")).hexdigest()
    encoded = json.dumps(payload, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode("utf-8")
    return "digest-" + hashlib.sha256(encoded).hexdigest()


HANDLERS = {"message.post", "message.ephemeral", "message.update", "message.delete", "reaction.add", "reaction.remove", "file.upload", "view.open", "view.push", "view.update", "view.publish", "callback.respond", "thread.status", "profile.status.set", "web_api"}


def validate_handlers(catalog):
    for identity, spec in catalog["operations"].items():
        handler = spec["handler_config"]
        if handler["operation"] not in HANDLERS:
            raise ValueError(f"Slack operation {identity} names unknown handler {handler['operation']!r}")
        if handler["operation"] != "web_api":
            require_keys(handler, {"operation"}, set(), f"Slack operation {identity} handler_config")
            continue
        require_keys(handler, {"operation", "method", "read_only"}, {"files", "params"}, f"Slack operation {identity} handler_config")
        if not isinstance(handler["method"], str) or not re.fullmatch(r"[a-z][A-Za-z]*(\.[a-z][A-Za-z]*){1,3}", handler["method"]):
            raise ValueError(f"Slack operation {identity} names no Web API method")
        require_bool(handler["read_only"], f"Slack operation {identity} handler_config.read_only")
        if "files" in handler:
            require_bool(handler["files"], f"Slack operation {identity} handler_config.files")
        if "params" in handler:
            require_object(handler["params"], f"Slack operation {identity} handler_config.params")
    return catalog


IMAGE_SIGNATURES = ((b"\x89PNG\r\n\x1a\n", "image/png"), (b"\xff\xd8\xff", "image/jpeg"))


def image_type(content):
    for signature, mime_type in IMAGE_SIGNATURES:
        if content.startswith(signature):
            return mime_type
    return "image/webp" if content[:4] == b"RIFF" and content[8:12] == b"WEBP" else None


def image_room(media, limits):
    used = sum(len(item["payload_base64"]) // 4 * 3 - item["payload_base64"][-2:].count("=") for item in media)
    return 0 if len(media) >= limits["max_images_per_event"] else max(0, limits["max_image_bytes_per_event"] - used)


def embed_image(media, row, path, content, limits):
    mime_type = image_type(content)
    if mime_type is None or len(content) > image_room(media, limits):
        return
    row["image"] = len(media)
    media.append({"mime_type": mime_type, "artifact": {"label": limits["file_label"], "payload_path": path, "payload_content_hash": octopus_content_hash(content)}, "payload_base64": base64.b64encode(content).decode("ascii")})
