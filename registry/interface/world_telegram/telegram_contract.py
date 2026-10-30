import base64
import hashlib
import ipaddress
import json
import os
import re
from datetime import datetime, timezone

from octopus_content_hash import octopus_content_hash
from telegram_render import safe_link_url


TOKEN_RE = re.compile(r"^[0-9]{1,20}:[A-Za-z0-9_-]{20,128}$")
CHAT_USERNAME_RE = re.compile(r"^@[A-Za-z][A-Za-z0-9_]{3,31}$")
NUMERIC_ID_RE = re.compile(r"^-?[1-9][0-9]{0,15}$")
MAX_ID = 2 ** 53
SUPPORTED_UPDATES = ("message", "edited_message", "callback_query", "message_reaction")
FILE_KINDS = {
    "animation": ("sendAnimation", "animation"),
    "audio": ("sendAudio", "audio"),
    "document": ("sendDocument", "document"),
    "photo": ("sendPhoto", "photo"),
    "video": ("sendVideo", "video"),
    "voice": ("sendVoice", "voice"),
}
MEDIA_FIELDS = (
    ("animation", ".mp4", "video/mp4"),
    ("video", ".mp4", "video/mp4"),
    ("video_note", ".mp4", "video/mp4"),
    ("voice", ".ogg", "audio/ogg"),
    ("audio", ".mp3", "audio/mpeg"),
    ("document", ".bin", "application/octet-stream"),
    ("sticker", ".webp", "image/webp"),
)
ENVIRONMENT_KEYS = ("bot_token_env", "allowed_user_ids_env", "home_chat_env")
HANDLERS = {"message.send", "message.edit", "message.delete", "reaction.set", "file.send", "chat.action", "bot_api"}


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


def require_text(value, name, maximum):
    if not isinstance(value, str) or not value.strip():
        raise ValueError(f"{name} must be non-empty text")
    if len(value) > maximum:
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
    for index, item in enumerate(value):
        item = require_string(item, f"{name}[{index}]")
        if item in result:
            raise ValueError(f"{name} repeats {item!r}")
        result.append(item)
    return result


def is_id(value):
    return isinstance(value, int) and not isinstance(value, bool) and value != 0 and abs(value) < MAX_ID


def require_user_id(value, name):
    if not is_id(value) or value < 0:
        raise ValueError(f"{name} must be a positive Telegram user id")
    return value


def require_chat_id(value, name):
    if isinstance(value, str):
        if CHAT_USERNAME_RE.fullmatch(value):
            return value
        if NUMERIC_ID_RE.fullmatch(value):
            value = int(value)
    if not is_id(value):
        raise ValueError(f"{name} must be a Telegram chat id or @username")
    return value


def parse_ids(raw, name, kind):
    values = set()
    for part in re.split(r"[\s,]+", raw.strip()) if raw.strip() else []:
        if not NUMERIC_ID_RE.fullmatch(part):
            raise ValueError(f"{name} contains {part!r}, which is not a numeric Telegram id")
        number = int(part)
        values.add(require_user_id(number, name) if kind == "user" else require_chat_id(number, name))
    return values


def env_ids(name, kind):
    return parse_ids(os.environ.get(name, ""), name, kind)


def env_flag(name):
    value = os.environ.get(name)
    if value is None or not value.strip():
        return None
    lowered = value.strip().lower()
    if lowered in {"1", "true", "yes", "on"}:
        return True
    if lowered in {"0", "false", "no", "off"}:
        return False
    raise ValueError(f"{name} must be a boolean")


def env_chat(name):
    raw = os.environ.get(name, "").strip()
    return require_chat_id(raw, name) if raw else None


def _id_list(values, name, kind):
    if not isinstance(values, list):
        raise ValueError(f"{name} must be an array")
    checked = [require_user_id(value, f"{name}[{index}]") if kind == "user" else require_chat_id(value, f"{name}[{index}]") for index, value in enumerate(values)]
    if any(isinstance(value, str) for value in checked) or len(set(checked)) != len(checked):
        raise ValueError(f"{name} must contain distinct numeric ids")
    return checked


def validate_environment(value):
    environment = require_keys(value, set(ENVIRONMENT_KEYS), set(), "Telegram interface environment")
    names = [require_string(environment[key], f"Telegram interface environment.{key}") for key in ENVIRONMENT_KEYS]
    if len(set(names)) != len(names) or any(not re.fullmatch(r"[A-Z_][A-Z0-9_]*", name) for name in names):
        raise ValueError("Telegram interface environment names must be distinct environment variable names")
    return environment


def validate_config(config):
    require_keys(config, {"schema_version", "ledger", "transport", "intake", "authz", "gating", "media", "outbound", "environment"}, set(), "Telegram config")
    if config["schema_version"] != 1:
        raise ValueError("Telegram config schema_version must be 1")
    validate_environment(config["environment"])
    ledger = require_keys(config["ledger"], {"max_row_bytes"}, set(), "Telegram ledger")
    require_int(ledger["max_row_bytes"], "Telegram ledger.max_row_bytes", 256, 16777216)
    transport = require_keys(
        config["transport"],
        {"api_base_env", "api_base_default", "proxy_env", "fallback_ips_env", "fallback_ips", "connect_timeout_ms", "request_timeout_ms", "max_response_bytes", "rate_limit_max_wait_ms", "rate_limit_retries", "read_retry_limit"},
        set(),
        "Telegram transport",
    )
    for key in ("api_base_env", "api_base_default", "proxy_env", "fallback_ips_env"):
        require_string(transport[key], f"Telegram transport.{key}")
    for address in require_string_list(transport["fallback_ips"], "Telegram transport.fallback_ips", True):
        parse_fallback_ip(address, "Telegram transport.fallback_ips")
    for key, low, high in (
        ("connect_timeout_ms", 100, 120000),
        ("request_timeout_ms", 1000, 600000),
        ("max_response_bytes", 1024, 67108864),
        ("rate_limit_max_wait_ms", 0, 600000),
        ("rate_limit_retries", 0, 10),
        ("read_retry_limit", 0, 10),
    ):
        require_int(transport[key], f"Telegram transport.{key}", low, high)
    intake = require_keys(
        config["intake"],
        {"event_label", "allowed_updates", "long_poll_timeout_s", "network_backoff_ms", "network_backoff_cap_ms", "conflict_backoff_ms", "conflict_backoff_cap_ms", "max_pending_events", "pending_retry_cap_ms", "pending_max_age_ms", "offset_max_idle_ms", "dedup_retention", "media_group_quiet_ms", "answer_callbacks", "reply_context_chars"},
        set(),
        "Telegram intake",
    )
    require_string(intake["event_label"], "Telegram intake.event_label")
    updates = require_string_list(intake["allowed_updates"], "Telegram intake.allowed_updates")
    unknown_updates = sorted(set(updates) - set(SUPPORTED_UPDATES))
    if unknown_updates:
        raise ValueError(f"Telegram intake.allowed_updates has unsupported kinds: {', '.join(unknown_updates)}")
    for key, low, high in (
        ("long_poll_timeout_s", 1, 50),
        ("network_backoff_ms", 10, 60000),
        ("network_backoff_cap_ms", 10, 600000),
        ("conflict_backoff_ms", 100, 120000),
        ("conflict_backoff_cap_ms", 100, 600000),
        ("max_pending_events", 1, 1000000),
        ("pending_retry_cap_ms", 100, 3600000),
        ("pending_max_age_ms", 60000, 604800000),
        ("offset_max_idle_ms", 3600000, 604800000),
        ("dedup_retention", 1, 1000000),
        ("media_group_quiet_ms", 0, 10000),
        ("reply_context_chars", 0, 4096),
    ):
        require_int(intake[key], f"Telegram intake.{key}", low, high)
    if intake["network_backoff_cap_ms"] < intake["network_backoff_ms"] or intake["conflict_backoff_cap_ms"] < intake["conflict_backoff_ms"]:
        raise ValueError("Telegram backoff caps must not be below their initial backoff")
    require_bool(intake["answer_callbacks"], "Telegram intake.answer_callbacks")
    authz = require_keys(config["authz"], {"allow_all_users", "allow_all_users_env", "allowed_user_ids", "allowed_chat_ids", "allowed_chat_ids_env", "deny_unmatched"}, set(), "Telegram authz")
    require_bool(authz["allow_all_users"], "Telegram authz.allow_all_users")
    require_bool(authz["deny_unmatched"], "Telegram authz.deny_unmatched")
    for key in ("allow_all_users_env", "allowed_chat_ids_env"):
        require_string(authz[key], f"Telegram authz.{key}")
    _id_list(authz["allowed_user_ids"], "Telegram authz.allowed_user_ids", "user")
    _id_list(authz["allowed_chat_ids"], "Telegram authz.allowed_chat_ids", "chat")
    gating = require_keys(
        config["gating"],
        {"respond_in_private", "groups_require_address", "address_by_mention", "address_by_reply", "address_by_command", "free_response_chat_ids", "free_response_chat_ids_env", "mention_patterns", "accept_message_edits", "accept_reactions", "accept_bot_messages"},
        set(),
        "Telegram gating",
    )
    for key in ("respond_in_private", "groups_require_address", "address_by_mention", "address_by_reply", "address_by_command", "accept_message_edits", "accept_reactions", "accept_bot_messages"):
        require_bool(gating[key], f"Telegram gating.{key}")
    _id_list(gating["free_response_chat_ids"], "Telegram gating.free_response_chat_ids", "chat")
    require_string(gating["free_response_chat_ids_env"], "Telegram gating.free_response_chat_ids_env")
    for index, pattern in enumerate(require_string_list(gating["mention_patterns"], "Telegram gating.mention_patterns", True)):
        try:
            re.compile(pattern, re.IGNORECASE)
        except re.error as error:
            raise ValueError(f"Telegram gating.mention_patterns[{index}] is invalid: {error}") from error
    media = require_keys(config["media"], {"download_enabled", "max_download_bytes", "max_upload_bytes", "max_files_per_event", "text_preview_bytes", "text_extensions", "retention_days", "file_label", "max_images_per_event", "max_image_bytes_per_event"}, set(), "Telegram media")
    require_bool(media["download_enabled"], "Telegram media.download_enabled")
    require_string(media["file_label"], "Telegram media.file_label", maximum=256)
    require_int(media["max_images_per_event"], "Telegram media.max_images_per_event", 0, 16)
    require_int(media["max_image_bytes_per_event"], "Telegram media.max_image_bytes_per_event", 0, 67108864)
    require_int(media["max_download_bytes"], "Telegram media.max_download_bytes", 1, 2147483648)
    require_int(media["max_upload_bytes"], "Telegram media.max_upload_bytes", 1, 2147483648)
    require_int(media["max_files_per_event"], "Telegram media.max_files_per_event", 0, 100)
    require_int(media["text_preview_bytes"], "Telegram media.text_preview_bytes", 0, 1048576)
    require_int(media["retention_days"], "Telegram media.retention_days", 1, 3650)
    for extension in require_string_list(media["text_extensions"], "Telegram media.text_extensions", True):
        if not re.fullmatch(r"\.[a-z0-9]{1,16}", extension):
            raise ValueError(f"Telegram media.text_extensions contains invalid extension {extension!r}")
    outbound = require_keys(
        config["outbound"],
        {"max_message_chars", "max_caption_chars", "max_text_chars", "max_messages_per_operation", "max_manifest_bytes", "max_payload_bytes", "max_buttons", "link_previews", "unsent_retry_window_ms", "unsent_retry_backoff_cap_ms", "bot_message_retention", "typing"},
        set(),
        "Telegram outbound",
    )
    for key, low, high in (
        ("max_message_chars", 16, 4096),
        ("max_caption_chars", 16, 4096),
        ("max_text_chars", 1, 1048576),
        ("max_messages_per_operation", 1, 100),
        ("max_manifest_bytes", 1, 16777216),
        ("max_payload_bytes", 1, 1073741824),
        ("max_buttons", 0, 100),
        ("unsent_retry_window_ms", 0, 86400000),
        ("unsent_retry_backoff_cap_ms", 100, 3600000),
        ("bot_message_retention", 1, 1000000),
    ):
        require_int(outbound[key], f"Telegram outbound.{key}", low, high)
    require_bool(outbound["link_previews"], "Telegram outbound.link_previews")
    typing = require_keys(outbound["typing"], {"enabled", "refresh_ms", "max_ms"}, set(), "Telegram outbound.typing")
    require_bool(typing["enabled"], "Telegram outbound.typing.enabled")
    require_int(typing["refresh_ms"], "Telegram outbound.typing.refresh_ms", 1000, 5000)
    require_int(typing["max_ms"], "Telegram outbound.typing.max_ms", 1000, 600000)
    return config


def parse_fallback_ip(value, name):
    try:
        address = ipaddress.ip_address(value)
    except ValueError as error:
        raise ValueError(f"{name} contains invalid address {value!r}") from error
    if address.version != 4 or address.is_private or address.is_loopback or address.is_link_local or address.is_unspecified or address.is_multicast:
        raise ValueError(f"{name} must contain public IPv4 addresses, not {value!r}")
    return str(address)


def access_from_environment(config):
    environment = config["environment"]
    authz = config["authz"]
    gating = config["gating"]
    override = env_flag(authz["allow_all_users_env"])
    return {
        "allow_all": authz["allow_all_users"] if override is None else override,
        "users": set(authz["allowed_user_ids"]) | env_ids(environment["allowed_user_ids_env"], "user"),
        "chats": set(authz["allowed_chat_ids"]) | env_ids(authz["allowed_chat_ids_env"], "chat"),
        "free_chats": set(gating["free_response_chat_ids"]) | env_ids(gating["free_response_chat_ids_env"], "chat"),
        "home_chat": env_chat(environment["home_chat_env"]),
    }


def authorize(config, access, chat_id, user_id):
    if access["allow_all"]:
        return True, "allow_all_users"
    if user_id is not None and user_id in access["users"]:
        return True, "allowed_user"
    if chat_id is not None and chat_id in access["chats"]:
        return True, "allowed_chat"
    if not config["authz"]["deny_unmatched"]:
        return True, "unmatched_allowed"
    return False, "user_not_allowed"


def utf16_index(text, offset):
    encoded = text.encode("utf-16-le")
    if offset < 0 or offset * 2 > len(encoded):
        return None
    try:
        return len(encoded[: offset * 2].decode("utf-16-le"))
    except UnicodeDecodeError:
        return None


def entity_span(text, entity):
    offset, length = entity.get("offset"), entity.get("length")
    if not isinstance(offset, int) or isinstance(offset, bool) or not isinstance(length, int) or isinstance(length, bool) or length <= 0:
        return None
    start, end = utf16_index(text, offset), utf16_index(text, offset + length)
    if start is None or end is None or end <= start:
        return None
    return start, end


def message_body(message):
    if isinstance(message.get("text"), str):
        entities = message.get("entities")
        return message["text"], [entity for entity in entities if isinstance(entity, dict)] if isinstance(entities, list) else []
    if isinstance(message.get("caption"), str):
        entities = message.get("caption_entities")
        return message["caption"], [entity for entity in entities if isinstance(entity, dict)] if isinstance(entities, list) else []
    return "", []


def readable_text(text, entities):
    insertions = []
    for entity in entities:
        span = entity_span(text, entity)
        if span is None:
            continue
        if entity.get("type") == "text_link" and isinstance(entity.get("url"), str):
            url = entity["url"].strip()
            if url and text[span[0] : span[1]].strip() != url:
                insertions.append((span[1], f" ({url})"))
        elif entity.get("type") == "text_mention" and isinstance(entity.get("user"), dict) and is_id(entity["user"].get("id")):
            insertions.append((span[1], f" (user {entity['user']['id']})"))
    for position, addition in sorted(insertions, key=lambda item: item[0], reverse=True):
        text = text[:position] + addition + text[position:]
    return text


def command_of(text, entities, bot_username):
    for entity in entities:
        if entity.get("type") != "bot_command" or entity.get("offset") != 0:
            continue
        span = entity_span(text, entity)
        if span is None:
            return None
        name, _, target = text[span[0] + 1 : span[1]].partition("@")
        if not re.fullmatch(r"[A-Za-z0-9_]{1,64}", name):
            return None
        return {"name": name.lower(), "args": text[span[1] :].strip(), "ours": not target or target.casefold() == bot_username.casefold()}
    return None


def mentions_bot(text, entities, bot):
    for entity in entities:
        if entity.get("type") == "mention":
            span = entity_span(text, entity)
            if span is not None and text[span[0] : span[1]].lstrip("@").casefold() == bot["username"].casefold():
                return True
        elif entity.get("type") == "text_mention" and isinstance(entity.get("user"), dict) and entity["user"].get("id") == bot["id"]:
            return True
    return False


def is_topic_root(message, reply):
    return reply.get("forum_topic_created") is not None or (message.get("is_topic_message") is True and reply.get("message_id") == message.get("message_thread_id"))


def replies_to_bot(message, bot):
    reply = message.get("reply_to_message")
    if not isinstance(reply, dict) or is_topic_root(message, reply):
        return False
    sender = reply.get("from")
    return isinstance(sender, dict) and sender.get("id") == bot["id"]


def topic_of(message):
    thread = message.get("message_thread_id")
    chat = message.get("chat") if isinstance(message.get("chat"), dict) else {}
    if is_id(thread) and thread > 0 and (message.get("is_topic_message") is True or chat.get("is_forum") is True):
        return thread
    return None


def sender_id(body):
    sender = body.get("from") if isinstance(body.get("from"), dict) else body.get("user")
    if isinstance(sender, dict) and is_id(sender.get("id")):
        return sender["id"]
    return None


def chat_of(body):
    chat = body.get("chat")
    if isinstance(chat, dict) and is_id(chat.get("id")):
        return chat
    message = body.get("message")
    if isinstance(message, dict) and isinstance(message.get("chat"), dict) and is_id(message["chat"].get("id")):
        return message["chat"]
    return None


def person(user):
    if not isinstance(user, dict) or not is_id(user.get("id")):
        return None
    view = {"id": user["id"]}
    name = " ".join(part.strip() for part in (user.get("first_name"), user.get("last_name")) if isinstance(part, str) and part.strip())
    if name:
        view["name"] = name
    if isinstance(user.get("username"), str) and user["username"]:
        view["username"] = user["username"]
    if user.get("is_bot") is True:
        view["is_bot"] = True
    return view


def chat_view(chat):
    view = {"id": chat["id"], "type": chat.get("type") if isinstance(chat.get("type"), str) else "unknown"}
    for key in ("title", "username"):
        if isinstance(chat.get(key), str) and chat[key]:
            view[key] = chat[key]
    if chat.get("is_forum") is True:
        view["is_forum"] = True
    return view


def iso_time(value):
    if isinstance(value, int) and not isinstance(value, bool) and value > 0:
        return datetime.fromtimestamp(value, timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
    return None


def message_address(config, access, bot, message):
    gating = config["gating"]
    chat = message.get("chat") if isinstance(message.get("chat"), dict) else {}
    sender = message.get("from") if isinstance(message.get("from"), dict) else {}
    if sender.get("is_bot") is True and not gating["accept_bot_messages"]:
        return None, "bot_message"
    if chat.get("type") == "private":
        return ("private_chat", None) if gating["respond_in_private"] else (None, "private_chats_disabled")
    if chat.get("type") not in {"group", "supergroup"}:
        return None, "chat_type_not_supported"
    text, entities = message_body(message)
    command = command_of(text, entities, bot["username"])
    if command is not None and not command["ours"]:
        return None, "command_for_another_bot"
    if chat.get("id") in access["free_chats"]:
        return "free_response_chat", None
    if not gating["groups_require_address"]:
        return "open_group", None
    if command is not None and gating["address_by_command"]:
        return "command", None
    if gating["address_by_mention"] and (mentions_bot(text, entities, bot) or any(re.search(pattern, text, re.IGNORECASE) for pattern in gating["mention_patterns"])):
        return "mention", None
    if gating["address_by_reply"] and replies_to_bot(message, bot):
        return "reply", None
    return None, "not_addressed_to_bot"


def media_items(message, max_bytes):
    items = []
    photos = message.get("photo")
    if isinstance(photos, list):
        sizes = [size for size in photos if isinstance(size, dict) and isinstance(size.get("file_id"), str)]
        fitting = [size for size in sizes if isinstance(size.get("file_size"), int) and size["file_size"] <= max_bytes]
        number = lambda value: value if isinstance(value, int) and not isinstance(value, bool) else 0
        if sizes:
            chosen = max(fitting or sizes, key=lambda size: (number(size.get("width")) * number(size.get("height")), number(size.get("file_size"))))
            items.append({"kind": "photo", "blob": chosen, "suffix": ".jpg", "mime_type": "image/jpeg"})
    seen = {item["blob"].get("file_unique_id") for item in items}
    for field, suffix, mime_type in MEDIA_FIELDS:
        blob = message.get(field)
        if not isinstance(blob, dict) or not isinstance(blob.get("file_id"), str):
            continue
        unique = blob.get("file_unique_id")
        if unique is not None and unique in seen:
            continue
        seen.add(unique)
        if field == "sticker" and blob.get("is_animated") is True:
            suffix, mime_type = ".tgs", "application/x-tgsticker"
        elif field == "sticker" and blob.get("is_video") is True:
            suffix, mime_type = ".webm", "video/webm"
        items.append({"kind": field, "blob": blob, "suffix": suffix, "mime_type": blob.get("mime_type") if isinstance(blob.get("mime_type"), str) else mime_type})
    return items


def forward_origin(origin):
    if not isinstance(origin, dict):
        return None
    kind = origin.get("type")
    if kind == "user":
        sender = person(origin.get("sender_user"))
        return (sender or {}).get("name") or (sender or {}).get("username")
    if kind == "hidden_user":
        return origin.get("sender_user_name") if isinstance(origin.get("sender_user_name"), str) else None
    chat = origin.get("sender_chat") if kind == "chat" else origin.get("chat") if kind == "channel" else None
    if isinstance(chat, dict):
        return chat.get("title") or chat.get("username")
    return None


IMAGE_SIGNATURES = ((b"\x89PNG\r\n\x1a\n", "image/png"), (b"\xff\xd8\xff", "image/jpeg"))


def image_type(content):
    for signature, mime_type in IMAGE_SIGNATURES:
        if content.startswith(signature):
            return mime_type
    return "image/webp" if content[:4] == b"RIFF" and content[8:12] == b"WEBP" else None


def image_room(media, limits):
    used = sum(len(item["payload_base64"]) // 4 * 3 - item["payload_base64"][-2:].count("=") for item in media)
    return 0 if len(media) >= limits["max_images_per_event"] else max(0, limits["max_image_bytes_per_event"] - used)


def embed_image(media, view, path, content, limits):
    mime_type = image_type(content)
    if mime_type is None or len(content) > image_room(media, limits):
        return
    view["image"] = len(media)
    media.append({"mime_type": mime_type, "artifact": {"label": limits["file_label"], "payload_path": path, "payload_content_hash": octopus_content_hash(content)}, "payload_base64": base64.b64encode(content).decode("ascii")})


def message_content(message, bot, files, config):
    text, entities = message_body(message)
    content = {}
    readable = readable_text(text, entities)
    if readable:
        content["text"] = readable
    command = command_of(text, entities, bot["username"])
    if command is not None:
        content["command"] = {"name": command["name"], "args": command["args"]}
    reply = message.get("reply_to_message")
    limit = config["intake"]["reply_context_chars"]
    if isinstance(reply, dict) and not is_topic_root(message, reply) and is_id(reply.get("message_id")):
        reply_text = readable_text(*message_body(reply))
        view = {"message_id": reply["message_id"]}
        sender = person(reply.get("from"))
        if sender is not None:
            view["from"] = sender
        if sender is not None and sender["id"] == bot["id"]:
            view["from_this_bot"] = True
        if reply_text and limit:
            view["text"] = reply_text if len(reply_text) <= limit else reply_text[:limit].rstrip() + "…"
        kinds = [item["kind"] for item in media_items(reply, config["media"]["max_download_bytes"])]
        if kinds:
            view["file_kinds"] = kinds
        content["reply_to"] = view
    quote = message.get("quote")
    if isinstance(quote, dict) and isinstance(quote.get("text"), str) and quote["text"]:
        content["quote"] = quote["text"]
    origin = forward_origin(message.get("forward_origin"))
    if origin:
        content["forwarded_from"] = origin
    if files:
        content["files"] = files
    location = message.get("venue", {}).get("location") if isinstance(message.get("venue"), dict) else message.get("location")
    if isinstance(location, dict) and isinstance(location.get("latitude"), (int, float)) and isinstance(location.get("longitude"), (int, float)):
        content["location"] = {"latitude": location["latitude"], "longitude": location["longitude"]}
        if isinstance(message.get("venue"), dict):
            for key in ("title", "address"):
                if isinstance(message["venue"].get(key), str):
                    content["location"][key] = message["venue"][key]
    contact = message.get("contact")
    if isinstance(contact, dict) and isinstance(contact.get("phone_number"), str):
        content["contact"] = {key: contact[key] for key in ("phone_number", "first_name", "last_name", "user_id") if contact.get(key) is not None}
    poll = message.get("poll")
    if isinstance(poll, dict) and isinstance(poll.get("question"), str):
        content["poll"] = {"question": poll["question"], "options": [option.get("text") for option in poll.get("options") or [] if isinstance(option, dict)]}
    dice = message.get("dice")
    if isinstance(dice, dict) and isinstance(dice.get("emoji"), str):
        content["dice"] = {"emoji": dice["emoji"], "value": dice.get("value")}
    if isinstance(message.get("media_group_id"), str):
        content["media_group_id"] = message["media_group_id"]
    return content


def reaction_names(reactions):
    names = []
    for reaction in reactions if isinstance(reactions, list) else []:
        if not isinstance(reaction, dict):
            continue
        if reaction.get("type") == "emoji" and isinstance(reaction.get("emoji"), str):
            names.append(reaction["emoji"])
        elif reaction.get("type") == "custom_emoji" and isinstance(reaction.get("custom_emoji_id"), str):
            names.append(f"custom:{reaction['custom_emoji_id']}")
        elif reaction.get("type") == "paid":
            names.append("paid")
    return names


def update_kind(update):
    for kind in SUPPORTED_UPDATES:
        if isinstance(update.get(kind), dict):
            return kind
    return None


def update_key(bot_id, update):
    kind = update_kind(update)
    body = update.get(kind) if kind else None
    if kind == "callback_query":
        identity = str(body.get("id"))
    elif kind is not None:
        chat = body.get("chat") if isinstance(body.get("chat"), dict) else {}
        identity = ":".join(str(part) for part in (chat.get("id"), body.get("message_id"), body.get("date"), body.get("edit_date"), sender_id(body)))
    else:
        identity = hashlib.sha256(json.dumps(update, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode("utf-8")).hexdigest()
    seed = f"{bot_id}:{update.get('update_id')}:{kind}:{identity}"
    return "update-" + hashlib.sha256(seed.encode("utf-8")).hexdigest()


def validate_buttons(value, config):
    if not isinstance(value, list):
        raise ValueError("Telegram buttons must be an array of rows; each row is an array of {text, data} or {text, url}")
    rows = []
    count = 0
    for row_index, row in enumerate(value):
        if not isinstance(row, list) or not row:
            raise ValueError("Telegram buttons must be an array of rows; each row is an array of {text, data} or {text, url}")
        rendered = []
        for index, button in enumerate(row):
            name = f"Telegram buttons[{row_index}][{index}]"
            button = require_keys(button, {"text"}, {"data", "url"}, name)
            text = require_string(button["text"], f"{name}.text", maximum=64)
            if ("data" in button) == ("url" in button):
                raise ValueError(f"{name} needs exactly one of data or url")
            if "data" in button:
                data = require_string(button["data"], f"{name}.data")
                if len(data.encode("utf-8")) > 64:
                    raise ValueError(f"{name}.data exceeds Telegram's 64-byte callback limit")
                rendered.append({"text": text, "callback_data": data})
            else:
                url = safe_link_url(button["url"])
                if url is None:
                    raise ValueError(f"{name}.url must be an http, https or tg link")
                rendered.append({"text": text, "url": url})
            count += 1
        rows.append(rendered)
    if count > config["outbound"]["max_buttons"]:
        raise ValueError(f"Telegram buttons exceed {config['outbound']['max_buttons']} buttons")
    return rows


def validate_handlers(catalog):
    for identity, spec in catalog["operations"].items():
        handler = spec["handler_config"]
        if handler["operation"] not in HANDLERS:
            raise ValueError(f"Telegram operation {identity} names unknown handler {handler['operation']!r}")
        if handler["operation"] != "bot_api":
            require_keys(handler, {"operation"}, set(), f"Telegram operation {identity} handler_config")
            continue
        require_keys(handler, {"operation", "method", "read_only"}, set(), f"Telegram operation {identity} handler_config")
        if not isinstance(handler["method"], str) or not re.fullmatch(r"[a-z][A-Za-z]{1,63}", handler["method"]):
            raise ValueError(f"Telegram operation {identity} names no Bot API method")
        require_bool(handler["read_only"], f"Telegram operation {identity} handler_config.read_only")
    return catalog


def validate_args(handler, args, config):
    for key in ("chat_id", "from_chat_id"):
        if key in args:
            args[key] = require_chat_id(args[key], f"Telegram {key}")
    if handler == "bot_api":
        return args
    outbound = config["outbound"]
    for key in ("text", "caption"):
        if key in args:
            require_text(args[key], f"Telegram {key}", outbound["max_text_chars"])
    if "buttons" in args:
        args["buttons"] = validate_buttons(args["buttons"], config)
    if handler == "message.edit" and "text" not in args and "buttons" not in args:
        raise ValueError("Telegram edit_message needs text, buttons or both")
    if handler == "chat.action":
        args.setdefault("action", "typing")
    if handler == "file.send":
        if ("blob_id" in args) == ("file_id" in args):
            raise ValueError("Telegram send_file needs exactly one of blob_id or file_id")
        if "blob_id" in args:
            filename = require_string(args.get("filename"), "Telegram send_file filename", maximum=255)
            if "/" in filename or "\\" in filename or filename in {".", ".."} or any(ord(character) < 32 for character in filename):
                raise ValueError("Telegram send_file filename must be a plain file name")
        elif "filename" in args:
            raise ValueError("Telegram send_file takes filename only with blob_id")
        if args.setdefault("kind", "document") not in FILE_KINDS:
            raise ValueError(f"Telegram send_file kind must be one of {', '.join(sorted(FILE_KINDS))}")
    return args
