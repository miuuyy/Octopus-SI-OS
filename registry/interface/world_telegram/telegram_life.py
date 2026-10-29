#!/usr/bin/env python3
import base64
import hashlib
import json
import math
import mimetypes
import os
import re
import shutil
import stat
import threading
import time
import urllib.parse

import telegram_boundary as boundary
from telegram_contract import FILE_KINDS
from telegram_contract import TOKEN_RE
from telegram_contract import access_from_environment
from telegram_contract import authorize
from telegram_contract import chat_of
from telegram_contract import chat_view
from telegram_contract import is_id
from telegram_contract import iso_time
from telegram_contract import media_items
from telegram_contract import message_address
from telegram_contract import message_body
from telegram_contract import message_content
from telegram_contract import normalize_operation
from telegram_contract import person
from telegram_contract import reaction_names
from telegram_contract import readable_text
from telegram_contract import require_int
from telegram_contract import require_keys
from telegram_contract import require_object
from telegram_contract import sender_id
from telegram_contract import topic_of
from telegram_contract import update_key
from telegram_contract import update_kind
from telegram_contract import validate_config
from telegram_contract import validate_environment
from telegram_render import render_chunks
from telegram_wire import NOT_SENT
from telegram_wire import BotApi
from telegram_wire import TelegramWireError


CONFIG_PATH = os.path.join(boundary.INTERFACE_DIR, "config.json")
DESCRIPTOR_PATH = os.path.join(boundary.INTERFACE_DIR, "interface.json")
PENDING_DIR = os.path.join(boundary.VAR_DIR, "pending-updates")
MEDIA_DIR = os.path.join(boundary.VAR_DIR, "media")
STATE_PATH = os.path.join(boundary.VAR_DIR, "telegram-state.json")
OFFSET_PATH = os.path.join(boundary.VAR_DIR, "intake-offset.json")
CONFIG_MAX_BYTES = 1024 * 1024
TOKEN_PATTERN = re.compile(r"(?:bot)?[0-9]{5,20}:[A-Za-z0-9_-]{30,128}")

for _directory in (PENDING_DIR, MEDIA_DIR):
    boundary.ensure_private_directory(_directory)


class OperationFailure(Exception):
    def __init__(self, kind, error, progress):
        super().__init__(str(error))
        self.kind = kind
        self.error = error
        self.progress = list(progress)


class RetryOutbound(Exception):
    def __init__(self, wait_s):
        super().__init__("Telegram did not receive the operation")
        self.wait_s = wait_s


def now_ms():
    return int(time.time() * 1000)


def load_config():
    config = require_object(json.loads(boundary.read_bounded_regular(CONFIG_PATH, CONFIG_MAX_BYTES, "interface config")), "Telegram config")
    descriptor = require_object(json.loads(boundary.read_bounded_regular(DESCRIPTOR_PATH, CONFIG_MAX_BYTES, "interface descriptor")), "Telegram interface descriptor")
    if descriptor.get("id") != boundary.INTERFACE_ID:
        raise ValueError("Telegram interface descriptor identity does not match its folder")
    private = require_object(require_object(descriptor.get("interface_driver_config"), "Telegram interface_driver_config").get("private_config"), "Telegram interface private_config")
    config["environment"] = validate_environment(private.get("environment"))
    return validate_config(config)


def make_sanitizer(config):
    secrets = set()
    token = os.environ.get(config["environment"]["bot_token_env"], "").strip()
    proxy = os.environ.get(config["transport"]["proxy_env"], "").strip()
    for value in (token, proxy):
        if len(value) >= 8:
            secrets.update({value, urllib.parse.quote(value, safe=""), urllib.parse.quote_plus(value, safe="")})
    if proxy:
        parsed = urllib.parse.urlsplit(proxy)
        if parsed.username is not None and parsed.password is not None:
            username, password = urllib.parse.unquote(parsed.username), urllib.parse.unquote(parsed.password)
            secrets.update(item for item in (parsed.password, password, f"{username}:{password}", base64.b64encode(f"{username}:{password}".encode("utf-8")).decode("ascii")) if len(item) >= 4)
    ordered = sorted(secrets, key=len, reverse=True)

    def sanitize(value):
        if isinstance(value, dict):
            return {sanitize(key): sanitize(item) for key, item in value.items()}
        if isinstance(value, (list, tuple)):
            return [sanitize(item) for item in value]
        if isinstance(value, BaseException):
            value = str(value)
        if not isinstance(value, str):
            return value
        for secret in ordered:
            value = value.replace(secret, "[secret-redacted]")
        value = TOKEN_PATTERN.sub("[telegram-token-redacted]", value)
        return re.sub(r"(https?://|socks5h?://)([^/@\s]+)@", r"\1[credentials-redacted]@", value)

    return sanitize


def resolve_token(config):
    token = os.environ.get(config["environment"]["bot_token_env"], "").strip()
    if not token:
        raise ValueError("Telegram bot token is not configured; connect Telegram in Applications")
    if not TOKEN_RE.fullmatch(token):
        raise ValueError("Telegram bot token is malformed; paste the token from @BotFather again")
    return token


def resolve_access(config):
    access = access_from_environment(config)
    if not access["allow_all"] and not access["users"] and not access["chats"]:
        raise ValueError("Telegram allowed users are not configured; connect Telegram and press Start in the bot, or enter user IDs")
    return access


def bot_identity(me):
    me = require_object(me, "Telegram getMe result")
    if me.get("is_bot") is not True or not is_id(me.get("id")) or not isinstance(me.get("username"), str) or not me["username"]:
        raise RuntimeError("Telegram getMe did not identify a bot with a username")
    return {"id": me["id"], "username": me["username"], "name": me.get("first_name") if isinstance(me.get("first_name"), str) else me["username"]}


def safe_file_name(name, fallback):
    base = os.path.basename(name) if isinstance(name, str) else ""
    cleaned = re.sub(r"[^A-Za-z0-9._-]", "_", base).strip("._")[:120]
    return cleaned or fallback


class Life:
    def __init__(self, config, access, bot, api):
        self.config = config
        self.access = access
        self.bot = bot
        self.api = api
        self.state_lock = threading.RLock()
        self.typing_lock = threading.Lock()
        self.typing = {}

    def load_state(self):
        value = boundary.read_json_file(STATE_PATH, 67108864, "state")
        if value is None:
            return {"schema_version": 1, "processed": [], "bot_messages": []}
        value = require_keys(value, {"schema_version", "processed", "bot_messages"}, set(), "Telegram state")
        if value["schema_version"] != 1 or any(not isinstance(value[key], list) or any(not isinstance(item, str) for item in value[key]) for key in ("processed", "bot_messages")):
            raise ValueError("Telegram state is malformed")
        return value

    def update_state(self, mutator):
        with self.state_lock:
            state = self.load_state()
            mutator(state)
            state["processed"] = state["processed"][-self.config["intake"]["dedup_retention"] :]
            state["bot_messages"] = state["bot_messages"][-self.config["outbound"]["bot_message_retention"] :]
            boundary.atomic_json(STATE_PATH, state)
            return state

    def load_offset(self):
        value = boundary.read_json_file(OFFSET_PATH, 65536, "intake offset")
        if value is None:
            return None
        value = require_keys(value, {"schema_version", "bot_id", "offset", "updated_at_unix_ms"}, set(), "Telegram intake offset")
        if value["schema_version"] != 1 or value["bot_id"] != self.bot["id"] or now_ms() - require_int(value["updated_at_unix_ms"], "Telegram intake offset time", 0) > self.config["intake"]["offset_max_idle_ms"]:
            return None
        return require_int(value["offset"], "Telegram intake offset", 0)

    def commit_offset(self, offset):
        boundary.atomic_json(OFFSET_PATH, {"schema_version": 1, "bot_id": self.bot["id"], "offset": offset, "updated_at_unix_ms": now_ms()})

    def pending_path(self, key):
        return os.path.join(PENDING_DIR, f"{key}.json")

    def persist_updates(self, updates):
        processed = set(self.load_state()["processed"])
        stored = 0
        for update in updates:
            key = update_key(self.bot["id"], update)
            if key in processed or os.path.exists(self.pending_path(key)):
                continue
            if len(os.listdir(PENDING_DIR)) >= self.config["intake"]["max_pending_events"]:
                raise RuntimeError("Telegram pending update ceiling reached; updates stay with Telegram until it drains")
            boundary.atomic_json(self.pending_path(key), {"schema_version": 1, "key": key, "update_id": update["update_id"], "update": update, "received_at_unix_ms": now_ms(), "attempts": 0, "next_attempt_at_unix_ms": 0})
            stored += 1
        return stored

    def load_pending(self):
        records = []
        for name in os.listdir(PENDING_DIR):
            if not name.endswith(".json") or name.startswith("."):
                continue
            path = os.path.join(PENDING_DIR, name)
            try:
                record = boundary.read_json_file(path, 16777216, "pending update")
                record = require_keys(record, {"schema_version", "key", "update_id", "update", "received_at_unix_ms", "attempts", "next_attempt_at_unix_ms"}, {"last_error"}, "Telegram pending update")
                if record["schema_version"] != 1 or f"{record['key']}.json" != name or not isinstance(record["update"], dict):
                    raise ValueError("Telegram pending update identity is malformed")
                records.append(record)
            except (ValueError, OSError) as error:
                boundary.stderr(f"Telegram pending update {name} is unreadable and stays for inspection: {error}", key=f"pending:{name}")
        return sorted(records, key=lambda record: (record["update_id"], record["key"]))

    def finish(self, records):
        keys = [record["key"] for record in records]

        def mark(state):
            state["processed"].extend(key for key in keys if key not in state["processed"])

        self.update_state(mark)
        for key in keys:
            try:
                os.unlink(self.pending_path(key))
            except FileNotFoundError:
                pass

    def defer(self, records, error):
        message = str(boundary.sanitize(str(error)))
        for record in records:
            record = dict(record)
            record["attempts"] += 1
            record["next_attempt_at_unix_ms"] = now_ms() + min(self.config["intake"]["pending_retry_cap_ms"], 1000 * 2 ** min(record["attempts"], 16))
            record["last_error"] = message[:2000]
            boundary.atomic_json(self.pending_path(record["key"]), record)
        boundary.stderr(f"Telegram update {records[0]['update_id']} retained after error: {message}", key=f"defer:{records[0]['key']}")

    def process_pending(self):
        records = self.load_pending()
        intake = self.config["intake"]
        now = now_ms()
        albums = {}
        for record in records:
            album = self.album_key(record["update"])
            if album is not None:
                albums.setdefault(album, []).append(record)
        blocked = set()
        handled = set()
        wake = None
        for record in records:
            if record["key"] in handled:
                continue
            chat = self.chat_key(record["update"])
            if now - record["received_at_unix_ms"] > intake["pending_max_age_ms"]:
                boundary.stderr(f"Telegram update {record['update_id']} expired after {record['attempts']} attempts: {record.get('last_error')}", key=f"expired:{record['key']}")
                handled.add(record["key"])
                self.finish([record])
                continue
            if chat in blocked:
                continue
            album = self.album_key(record["update"])
            unit = albums[album] if album is not None else [record]
            due = max(item["next_attempt_at_unix_ms"] for item in unit)
            if album is not None:
                due = max(due, max(item["received_at_unix_ms"] for item in unit) + intake["media_group_quiet_ms"])
            if due > now:
                blocked.add(chat)
                wake = due if wake is None else min(wake, due)
                continue
            handled.update(item["key"] for item in unit)
            try:
                self.process_unit(unit)
            except Exception as error:
                self.defer(unit, error)
                blocked.add(chat)
                retry = now_ms() + 1000
                wake = retry if wake is None else min(wake, retry)
        return wake

    def album_key(self, update):
        kind = update_kind(update)
        message = update.get(kind) if kind in {"message", "edited_message"} else None
        if isinstance(message, dict) and kind == "message" and isinstance(message.get("media_group_id"), str) and isinstance(message.get("chat"), dict):
            return f"{message['chat'].get('id')}:{message['media_group_id']}"
        return None

    def chat_key(self, update):
        kind = update_kind(update)
        chat = chat_of(update.get(kind)) if kind else None
        return chat["id"] if chat else None

    def process_unit(self, unit):
        update = unit[0]["update"]
        kind = update_kind(update)
        if kind in {"message", "edited_message"}:
            event, typing = self.message_event(unit, kind)
        elif kind == "callback_query":
            event, typing = self.button_event(unit[0])
        elif kind == "message_reaction":
            event, typing = self.reaction_event(unit[0]), None
        else:
            event, typing = None, None
        if event is not None:
            boundary.hand_json_idempotent(self.config["intake"]["event_label"], event, f"telegram-event-{event['event_key']}", self.config)
        self.finish(unit)
        if event is not None and typing is not None:
            self.typing_start(*typing)

    def unit_key(self, unit):
        if len(unit) == 1:
            return unit[0]["key"]
        return "album-" + hashlib.sha256(",".join(sorted(record["key"] for record in unit)).encode("utf-8")).hexdigest()

    def refuse(self, update_id, reason, chat_id=None, user_id=None):
        scope = f"{chat_id}:{user_id}" if reason == "user_not_allowed" else f"{chat_id}"
        boundary.stderr(f"Telegram update {update_id} not admitted: {reason} (chat {chat_id}, user {user_id})", key=f"refuse:{reason}:{scope}")
        return None

    def message_event(self, unit, kind):
        messages = [record["update"][kind] for record in unit]
        first = messages[0]
        chat = first.get("chat")
        if not isinstance(chat, dict) or not is_id(chat.get("id")) or not is_id(first.get("message_id")):
            return self.refuse(unit[0]["update_id"], "malformed_message"), None
        user_id = sender_id(first)
        allowed, reason = authorize(self.config, self.access, chat["id"], user_id)
        if not allowed:
            return self.refuse(unit[0]["update_id"], reason, chat["id"], user_id), None
        if kind == "edited_message" and not self.config["gating"]["accept_message_edits"]:
            return self.refuse(unit[0]["update_id"], "message_edits_disabled", chat["id"], user_id), None
        addressed = [message_address(self.config, self.access, self.bot, message) for message in messages]
        addressed_by = next((address for address, _ in addressed if address is not None), None)
        if addressed_by is None:
            return self.refuse(unit[0]["update_id"], addressed[0][1], chat["id"], user_id), None
        key = self.unit_key(unit)
        files = self.download_files(messages, key)
        primary = next((message for message in messages if message_body(message)[0]), first)
        content = message_content(primary, self.bot, files, self.config)
        if len(messages) > 1:
            content["album_message_ids"] = [message["message_id"] for message in messages]
        if not content:
            return self.refuse(unit[0]["update_id"], "service_message", chat["id"], user_id), None
        event = {
            "schema_version": 1,
            "event_key": key,
            "event": "message" if kind == "message" else "message_edited",
            "addressed_by": addressed_by,
            "chat": chat_view(chat),
            "message_id": first["message_id"],
            "message": content,
        }
        topic = topic_of(first)
        if topic is not None:
            event["topic_id"] = topic
        sender = person(first.get("from"))
        if sender is not None:
            event["from"] = sender
        if isinstance(first.get("sender_chat"), dict) and is_id(first["sender_chat"].get("id")):
            event["sender_chat"] = chat_view(first["sender_chat"])
        for field, value in (("sent_at", iso_time(first.get("date"))), ("edited_at", iso_time(first.get("edit_date")))):
            if value is not None:
                event[field] = value
        return event, ((chat["id"], topic) if kind == "message" else None)

    def button_event(self, record):
        callback = record["update"]["callback_query"]
        if self.config["intake"]["answer_callbacks"] and isinstance(callback.get("id"), str):
            try:
                self.api.call("answerCallbackQuery", {"callback_query_id": callback["id"]}, timeout=15)
            except TelegramWireError as error:
                boundary.stderr(f"Telegram button press {record['update_id']} could not be acknowledged: {error}", key=f"callback:{record['key']}")
        message = callback.get("message") if isinstance(callback.get("message"), dict) else {}
        chat = message.get("chat")
        user_id = sender_id(callback)
        if not isinstance(chat, dict) or not is_id(chat.get("id")) or not is_id(message.get("message_id")) or not isinstance(callback.get("data"), str):
            return self.refuse(record["update_id"], "button_without_message_or_data", user_id=user_id), None
        allowed, reason = authorize(self.config, self.access, chat["id"], user_id)
        if not allowed:
            return self.refuse(record["update_id"], reason, chat["id"], user_id), None
        button = {"data": callback["data"]}
        text = readable_text(*message_body(message)) if message.get("date") else ""
        limit = self.config["intake"]["reply_context_chars"]
        if text and limit:
            button["message_text"] = text if len(text) <= limit else text[:limit].rstrip() + "…"
        event = {"schema_version": 1, "event_key": record["key"], "event": "button", "addressed_by": "button", "chat": chat_view(chat), "message_id": message["message_id"], "button": button}
        topic = topic_of(message)
        if topic is not None:
            event["topic_id"] = topic
        sender = person(callback.get("from"))
        if sender is not None:
            event["from"] = sender
        return event, (chat["id"], topic)

    def reaction_event(self, record):
        reaction = record["update"]["message_reaction"]
        chat = reaction.get("chat")
        user_id = sender_id(reaction)
        if not self.config["gating"]["accept_reactions"]:
            return self.refuse(record["update_id"], "reactions_disabled", user_id=user_id)
        if not isinstance(chat, dict) or not is_id(chat.get("id")) or not is_id(reaction.get("message_id")):
            return self.refuse(record["update_id"], "malformed_reaction", user_id=user_id)
        allowed, reason = authorize(self.config, self.access, chat["id"], user_id)
        if not allowed:
            return self.refuse(record["update_id"], reason, chat["id"], user_id)
        if f"{chat['id']}:{reaction['message_id']}" not in self.load_state()["bot_messages"]:
            return self.refuse(record["update_id"], "reaction_to_message_not_from_this_bot", chat["id"], user_id)
        old, new = reaction_names(reaction.get("old_reaction")), reaction_names(reaction.get("new_reaction"))
        change = {"added": [name for name in new if name not in old], "removed": [name for name in old if name not in new]}
        if not change["added"] and not change["removed"]:
            return self.refuse(record["update_id"], "reaction_unchanged", chat["id"], user_id)
        event = {"schema_version": 1, "event_key": record["key"], "event": "reaction", "addressed_by": "reaction", "chat": chat_view(chat), "message_id": reaction["message_id"], "reaction": change}
        sender = person(reaction.get("user"))
        if sender is not None:
            event["from"] = sender
        elif isinstance(reaction.get("actor_chat"), dict) and is_id(reaction["actor_chat"].get("id")):
            event["sender_chat"] = chat_view(reaction["actor_chat"])
        if iso_time(reaction.get("date")):
            event["sent_at"] = iso_time(reaction.get("date"))
        return event

    def download_files(self, messages, key):
        media = self.config["media"]
        items = [item for message in messages for item in media_items(message, media["max_download_bytes"])]
        files = [self.download_file(item, key, index) for index, item in enumerate(items[: media["max_files_per_event"]])]
        if len(items) > len(files):
            files.append({"kind": "omitted", "count": len(items) - len(files)})
        return files

    def download_file(self, item, key, index):
        media = self.config["media"]
        blob = item["blob"]
        name = blob.get("file_name") if isinstance(blob.get("file_name"), str) and blob["file_name"] else f"{item['kind']}-{index + 1}{item['suffix']}"
        view = {"kind": item["kind"], "name": name, "mime_type": item["mime_type"], "file_id": blob["file_id"]}
        for field in ("width", "height", "duration", "file_size"):
            if isinstance(blob.get(field), int) and not isinstance(blob.get(field), bool):
                view["size" if field == "file_size" else field] = blob[field]
        if item["kind"] == "sticker":
            for field in ("emoji", "set_name"):
                if isinstance(blob.get(field), str):
                    view[field] = blob[field]
        if not media["download_enabled"]:
            view["skipped"] = "download_disabled"
            return view
        if view.get("size", 0) > media["max_download_bytes"]:
            view["skipped"] = "too_large"
            return view
        try:
            info = require_object(self.api.call("getFile", {"file_id": blob["file_id"]}, retry_safe=True), "Telegram getFile result")
            if isinstance(info.get("file_size"), int) and info["file_size"] > media["max_download_bytes"]:
                view["skipped"] = "too_large"
                return view
            content = self.api.download(info.get("file_path"), media["max_download_bytes"])
        except TelegramWireError as error:
            if error.category == "response_too_large" or (error.category == "telegram_error" and "too big" in (error.description or "").lower()):
                view["skipped"] = "too_large"
                return view
            if error.category in {"telegram_error", "boundary_rejected"}:
                view["download_error"] = str(error)
                return view
            raise
        directory = boundary.ensure_private_directory(os.path.join(MEDIA_DIR, key))
        path = os.path.join(directory, f"{index + 1:02d}-{safe_file_name(name, item['kind'] + item['suffix'])}")
        boundary.atomic_bytes(path, content)
        view.update({"path": path, "size": len(content), "sha256": hashlib.sha256(content).hexdigest()})
        textual = item["mime_type"].startswith("text/") or os.path.splitext(name)[1].lower() in media["text_extensions"]
        if textual and len(content) <= media["text_preview_bytes"]:
            try:
                view["text_preview"] = content.decode("utf-8")
            except UnicodeDecodeError:
                pass
        return view

    def cleanup_media(self):
        cutoff = time.time() - self.config["media"]["retention_days"] * 86400
        for name in os.listdir(MEDIA_DIR):
            path = os.path.join(MEDIA_DIR, name)
            status = os.lstat(path)
            if stat.S_ISDIR(status.st_mode) and status.st_mtime < cutoff:
                shutil.rmtree(path)

    def typing_start(self, chat_id, topic_id):
        typing = self.config["outbound"]["typing"]
        if typing["enabled"]:
            with self.typing_lock:
                self.typing[(chat_id, topic_id)] = {"next": 0.0, "deadline": time.monotonic() + typing["max_ms"] / 1000.0}

    def typing_stop(self, chat_id, topic_id=None):
        with self.typing_lock:
            for key in [key for key in self.typing if key[0] == chat_id and (topic_id is None or key[1] == topic_id)]:
                self.typing.pop(key, None)

    def typing_loop(self):
        refresh = self.config["outbound"]["typing"]["refresh_ms"] / 1000.0
        while True:
            time.sleep(0.25)
            now = time.monotonic()
            with self.typing_lock:
                for key in [key for key, entry in self.typing.items() if entry["deadline"] <= now]:
                    self.typing.pop(key)
                due = [key for key, entry in self.typing.items() if entry["next"] <= now]
            for chat_id, topic_id in due:
                params = {"chat_id": chat_id, "action": "typing"}
                if topic_id is not None:
                    params["message_thread_id"] = topic_id
                try:
                    self.api.call("sendChatAction", params, timeout=10)
                except TelegramWireError:
                    self.typing_stop(chat_id, topic_id)
                    continue
                with self.typing_lock:
                    if (chat_id, topic_id) in self.typing:
                        self.typing[(chat_id, topic_id)]["next"] = now + refresh

    def target(self, args):
        if "chat_id" in args:
            return args["chat_id"]
        if self.access["home_chat"] is not None:
            return self.access["home_chat"]
        if len(self.access["users"]) == 1:
            return next(iter(self.access["users"]))
        raise ValueError("Telegram operation names no chat_id, and no home chat or single allowed user is configured")

    def deliver(self, method, params, progress, files=None, retry_safe=False):
        try:
            if files:
                return self.api.call_multipart(method, params, files)
            return self.api.call(method, params, retry_safe=retry_safe)
        except TelegramWireError as error:
            kind = "not_delivered" if error.category in NOT_SENT else "refused" if error.category in {"telegram_error", "unauthorized", "conflict", "boundary_rejected"} else "uncertain"
            raise OperationFailure(kind, error, progress) from None

    def remember(self, message):
        message = message if isinstance(message, dict) else {}
        chat = message.get("chat") if isinstance(message.get("chat"), dict) else {}
        if is_id(chat.get("id")) and is_id(message.get("message_id")):
            key = f"{chat['id']}:{message['message_id']}"

            def add(state):
                if key not in state["bot_messages"]:
                    state["bot_messages"].append(key)

            self.update_state(add)
        return message.get("message_id")

    def message_params(self, args, chat):
        params = {"chat_id": chat}
        if "topic_id" in args:
            params["message_thread_id"] = args["topic_id"]
        if args.get("silent"):
            params["disable_notification"] = True
        return params

    def single_chunk(self, text, text_format, limit, name):
        chunks = render_chunks(text, text_format, limit)
        if len(chunks) != 1:
            raise ValueError(f"Telegram {name} must fit one message of {limit} characters after formatting")
        return chunks[0]

    def execute(self, operation, args):
        chat = None if operation == "api.call" else self.target(args)
        try:
            return self.perform(operation, args, chat)
        finally:
            if chat is not None and operation != "chat.action":
                self.typing_stop(chat, args.get("topic_id"))

    def perform(self, operation, args, chat):
        outbound = self.config["outbound"]
        text_format = args.get("format", "markdown")
        if operation == "message.send":
            chunks = render_chunks(args["text"], text_format, outbound["max_message_chars"])
            if len(chunks) > outbound["max_messages_per_operation"]:
                raise ValueError(f"Telegram message needs {len(chunks)} messages, above the {outbound['max_messages_per_operation']} allowed per operation")
            progress = []
            for index, chunk in enumerate(chunks):
                params = self.message_params(args, chat)
                params.update({"text": chunk, "parse_mode": "HTML"})
                if index == 0 and "reply_to_message_id" in args:
                    params["reply_parameters"] = {"message_id": args["reply_to_message_id"], "allow_sending_without_reply": True}
                if index == len(chunks) - 1 and args.get("buttons"):
                    params["reply_markup"] = {"inline_keyboard": args["buttons"]}
                if not args.get("link_preview", outbound["link_previews"]):
                    params["link_preview_options"] = {"is_disabled": True}
                progress.append(self.remember(self.deliver("sendMessage", params, progress)))
            return {"operation": operation, "chat_id": chat, "message_ids": progress}
        if operation == "message.edit":
            params = {"chat_id": chat, "message_id": args["message_id"]}
            if "buttons" in args:
                params["reply_markup"] = {"inline_keyboard": args["buttons"]}
            method = "editMessageReplyMarkup"
            if "text" in args:
                method = "editMessageText"
                params.update({"text": self.single_chunk(args["text"], text_format, outbound["max_message_chars"], "message.edit text"), "parse_mode": "HTML"})
                if not args.get("link_preview", outbound["link_previews"]):
                    params["link_preview_options"] = {"is_disabled": True}
            try:
                self.deliver(method, params, [])
            except OperationFailure as failure:
                if failure.kind == "refused" and "message is not modified" in (failure.error.description or ""):
                    return {"operation": operation, "chat_id": chat, "message_id": args["message_id"], "unchanged": True}
                raise
            return {"operation": operation, "chat_id": chat, "message_id": args["message_id"]}
        if operation == "message.delete":
            self.deliver("deleteMessage", {"chat_id": chat, "message_id": args["message_id"]}, [])
            return {"operation": operation, "chat_id": chat, "message_id": args["message_id"]}
        if operation == "reaction.set":
            emoji = args.get("emoji")
            params = {"chat_id": chat, "message_id": args["message_id"], "reaction": [{"type": "emoji", "emoji": emoji}] if emoji else []}
            if args.get("big"):
                params["is_big"] = True
            self.deliver("setMessageReaction", params, [])
            return {"operation": operation, "chat_id": chat, "message_id": args["message_id"], "emoji": emoji}
        if operation == "file.send":
            method, field = FILE_KINDS[args["kind"]]
            params = self.message_params(args, chat)
            if "reply_to_message_id" in args:
                params["reply_parameters"] = {"message_id": args["reply_to_message_id"], "allow_sending_without_reply": True}
            if args.get("buttons"):
                params["reply_markup"] = {"inline_keyboard": args["buttons"]}
            if "caption" in args:
                params.update({"caption": self.single_chunk(args["caption"], text_format, outbound["max_caption_chars"], "file.send caption"), "parse_mode": "HTML"})
            if "file_id" in args:
                params[field] = args["file_id"]
                message = self.deliver(method, params, [])
            else:
                content = boundary.artifact_blob(args["blob_id"], self.config["media"]["max_upload_bytes"])
                mime_type = mimetypes.guess_type(args["filename"])[0] or "application/octet-stream"
                message = self.deliver(method, params, [], files={field: (args["filename"], mime_type, content)})
            return {"operation": operation, "chat_id": chat, "message_ids": [self.remember(message)], "kind": args["kind"]}
        if operation == "chat.action":
            params = {"chat_id": chat, "action": args["action"]}
            if "topic_id" in args:
                params["message_thread_id"] = args["topic_id"]
            self.deliver("sendChatAction", params, [])
            return {"operation": operation, "chat_id": chat, "action": args["action"]}
        if operation == "api.call":
            result = self.deliver(args["method"], args["params"], [], retry_safe=args["method"] in outbound["retry_safe_api_methods"])
            return {"operation": operation, "method": args["method"], "result": result}
        raise ValueError(f"Telegram operation {operation!r} is unsupported")

    def prepare_output(self, label, payload):
        if label != self.config["outbound"]["operation_label"]:
            raise ValueError(f"Telegram interface accepts only {self.config['outbound']['operation_label']!r} artifacts, not {label!r}")
        try:
            body = json.loads(payload)
        except (UnicodeDecodeError, json.JSONDecodeError) as error:
            raise ValueError(f"Telegram outbound payload is invalid JSON: {error}") from error
        return normalize_operation(body, self.config)

    def process_outbound(self, source_seq, content, run, first_attempt_ms):
        if content is None or run is None:
            boundary.hand_failure(source_seq, "poisoned_outbound", {"error": "movement carries no artifact or runtime run"}, f"telegram-poison-{source_seq}")
            return
        label, payload, content_hash = boundary.materialize_movement_payload(content, self.config)
        effect = boundary.load_effect(source_seq)
        if effect is not None and effect["source_artifact_hash"] != content_hash:
            boundary.hand_failure(source_seq, "effect_journal_mismatch", {"expected": effect["source_artifact_hash"], "observed": content_hash}, f"telegram-effect-mismatch-{source_seq}")
            return
        if effect is not None and effect["status"] in {"evidenced", "failure_evidenced"}:
            return
        if effect is not None and effect["status"] == "succeeded":
            effect.update({"status": "evidenced", "evidence_movement_seq": self.delivery_evidence(source_seq, content_hash, run, effect["result"])})
            boundary.commit_effect(source_seq, effect)
            return
        if effect is not None:
            evidence = boundary.hand_failure(source_seq, "indeterminate_external_effect", {"source_artifact_hash": content_hash, "error": effect.get("error") or "the interface stopped before Telegram answered"}, f"telegram-indeterminate-{source_seq}")
            effect.update({"status": "failure_evidenced", "evidence_movement_seq": evidence})
            boundary.commit_effect(source_seq, effect)
            return
        try:
            operation, args = self.prepare_output(label, payload)
        except Exception as error:
            boundary.hand_failure(source_seq, "boundary_rejected", {"source_artifact_hash": content_hash, "error": str(error)}, f"telegram-boundary-{source_seq}")
            return
        effect = {"schema_version": 1, "source_movement_seq": source_seq, "source_artifact_hash": content_hash, "run": run, "artifact_label": label, "status": "started", "operation": operation}
        boundary.commit_effect(source_seq, effect)
        details = {"source_artifact_hash": content_hash, "operation": operation}
        try:
            result = self.execute(operation, args)
        except ValueError as error:
            details["error"] = str(error)
            evidence = boundary.hand_failure(source_seq, "boundary_rejected", details, f"telegram-boundary-{source_seq}")
            effect.update({"status": "failure_evidenced", "error": str(error), "evidence_movement_seq": evidence})
            boundary.commit_effect(source_seq, effect)
            return
        except OperationFailure as failure:
            window = self.config["outbound"]["unsent_retry_window_ms"]
            if failure.kind == "not_delivered" and not failure.progress and now_ms() - first_attempt_ms < window:
                boundary.discard_effect(source_seq)
                raise RetryOutbound(failure.error.retry_after)
            details.update({"error": failure.error.details(), "delivered_message_ids": failure.progress})
            if failure.kind == "uncertain":
                effect.update({"status": "indeterminate", "error": str(failure.error)})
                boundary.commit_effect(source_seq, effect)
                failure_class = "indeterminate_external_effect"
            else:
                failure_class = "partial_delivery" if failure.progress else "telegram_unreachable" if failure.kind == "not_delivered" else "telegram_refused"
            evidence = boundary.hand_failure(source_seq, failure_class, details, f"telegram-failure-{source_seq}")
            effect.update({"status": "failure_evidenced", "error": str(failure.error), "evidence_movement_seq": evidence})
            boundary.commit_effect(source_seq, effect)
            return
        except Exception as error:
            effect.update({"status": "indeterminate", "error": str(error)})
            boundary.commit_effect(source_seq, effect)
            details["error"] = str(error)
            evidence = boundary.hand_failure(source_seq, "indeterminate_external_effect", details, f"telegram-indeterminate-{source_seq}")
            effect.update({"status": "failure_evidenced", "evidence_movement_seq": evidence})
            boundary.commit_effect(source_seq, effect)
            return
        effect.update({"status": "succeeded", "result": result})
        boundary.commit_effect(source_seq, effect)
        effect.update({"status": "evidenced", "evidence_movement_seq": self.delivery_evidence(source_seq, content_hash, run, result)})
        boundary.commit_effect(source_seq, effect)

    def delivery_evidence(self, source_seq, content_hash, run, result):
        return boundary.hand_json(self.config["outbound"]["delivery_label"], {"source_movement_seq": source_seq, "source_artifact_hash": content_hash, "run": run, "ok": True, "result": result}, f"telegram-delivery-{source_seq}")

    def outbound_loop(self):
        next_seq = boundary.load_cursor()
        retry = {"seq": None, "first": 0, "delay": 1.0}
        cap = self.config["outbound"]["unsent_retry_backoff_cap_ms"] / 1000.0
        while True:
            for source_seq, content, run in boundary.read_outbound(next_seq, self.config):
                if retry["seq"] != source_seq:
                    retry = {"seq": source_seq, "first": now_ms(), "delay": 1.0}
                try:
                    self.process_outbound(source_seq, content, run, retry["first"])
                except RetryOutbound as signal:
                    wait = min(cap, max(retry["delay"], float(signal.wait_s or 0)))
                    boundary.stderr(f"Telegram did not receive outbound movement {source_seq}; retrying while it stays unsent", key=f"retry:{source_seq}")
                    time.sleep(wait)
                    retry["delay"] = min(cap, retry["delay"] * 2)
                    break
                except Exception as error:
                    boundary.stderr(f"Telegram outbound movement {source_seq} failed: {error}", key=f"outbound:{source_seq}")
                    time.sleep(0.25)
                    break
                next_seq = source_seq + 1
                boundary.commit_cursor(next_seq)
            time.sleep(0.05)

    def take_over_webhook(self):
        info = self.api.call("getWebhookInfo", retry_safe=True)
        if isinstance(info, dict) and info.get("url"):
            self.api.call("deleteWebhook", {"drop_pending_updates": False})
            boundary.stderr("Telegram bot delivered its updates to a webhook; Octopus removed the webhook to receive them by long polling", key="webhook")

    def intake_loop(self):
        intake = self.config["intake"]
        offset = self.load_offset()
        network = intake["network_backoff_ms"] / 1000.0
        conflict = intake["conflict_backoff_ms"] / 1000.0
        last_cleanup = 0.0
        while True:
            wake = self.process_pending()
            if time.time() - last_cleanup > 3600:
                self.cleanup_media()
                last_cleanup = time.time()
            timeout = intake["long_poll_timeout_s"]
            if wake is not None:
                timeout = max(0, min(timeout, math.ceil((wake - now_ms()) / 1000.0)))
            params = {"timeout": timeout, "allowed_updates": intake["allowed_updates"]}
            if offset is not None:
                params["offset"] = offset
            try:
                updates = self.api.call("getUpdates", params, timeout=timeout + 15)
            except TelegramWireError as error:
                if error.category == "unauthorized":
                    raise RuntimeError("Telegram rejected the bot token; reconnect Telegram in Applications") from None
                if error.category == "conflict" and "webhook" in (error.description or "").lower():
                    self.take_over_webhook()
                    continue
                if error.category == "conflict":
                    boundary.stderr("Telegram reports another program receiving this bot's updates; polling retries with backoff. Stop the other program or Home that uses this bot token.", key="poll:conflict")
                    time.sleep(conflict)
                    conflict = min(intake["conflict_backoff_cap_ms"] / 1000.0, conflict * 2)
                    continue
                wait = float(error.retry_after) if error.category == "rate_limited" and error.retry_after is not None else network
                boundary.stderr(f"Telegram polling pauses and retries: {error}", key=f"poll:{error.category}")
                time.sleep(wait)
                network = min(intake["network_backoff_cap_ms"] / 1000.0, network * 2)
                continue
            if network != intake["network_backoff_ms"] / 1000.0 or conflict != intake["conflict_backoff_ms"] / 1000.0:
                boundary.forget("poll:")
            network = intake["network_backoff_ms"] / 1000.0
            conflict = intake["conflict_backoff_ms"] / 1000.0
            if not isinstance(updates, list):
                raise RuntimeError("Telegram getUpdates returned a non-array")
            received = sorted((update for update in updates if isinstance(update, dict) and is_id(update.get("update_id")) and update["update_id"] > 0), key=lambda update: update["update_id"])
            try:
                self.persist_updates(received)
            except Exception as error:
                boundary.stderr(f"Telegram updates stay with Telegram until they can be stored: {error}", key="persist")
                time.sleep(network)
                continue
            if received:
                offset = received[-1]["update_id"] + 1
                self.commit_offset(offset)


def main():
    config = load_config()
    boundary.SANITIZE[0] = make_sanitizer(config)
    token = resolve_token(config)
    access = resolve_access(config)
    api = BotApi(config, token, boundary.sanitize)
    boundary.start_runtime_verdict_reader()
    life = Life(config, access, bot_identity(api.call("getMe", retry_safe=True)), api)
    life.take_over_webhook()
    threading.Thread(target=life.outbound_loop, daemon=True).start()
    threading.Thread(target=life.typing_loop, daemon=True).start()
    boundary.signal_ready()
    life.intake_loop()


if __name__ == "__main__":
    try:
        main()
    except Exception as error:
        boundary.stderr(f"Telegram interface exited: {error}")
        raise SystemExit(1)
