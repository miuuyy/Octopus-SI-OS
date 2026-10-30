import base64
import binascii
import datetime
import hashlib
import html
import mimetypes
import re
from email import policy
from email.header import decode_header
from email.headerregistry import Address
from email.message import EmailMessage
from email.parser import BytesHeaderParser
from email.parser import BytesParser
from email.utils import format_datetime
from email.utils import getaddresses
from email.utils import parsedate_to_datetime
from html.parser import HTMLParser

from octopus_content_hash import octopus_content_hash


class ContractError(ValueError):
    pass


ENVIRONMENT_KEYS = ("address_env", "password_env", "imap_host_env", "smtp_host_env", "allowed_senders_env")
OPERATIONS = ("message.send", "message.reply", "message.forward", "message.mark", "message.move", "message.search", "message.fetch", "mailbox.list")
SENDING_OPERATIONS = frozenset({"message.send", "message.reply", "message.forward"})
SENT_COPY_MODES = frozenset({"provider", "append", "none"})
TLS_MODES = frozenset({"implicit", "starttls"})
SUMMARY_HEADERS = (
    "From", "Sender", "Reply-To", "To", "Cc", "Subject", "Date", "Message-ID", "In-Reply-To", "References",
    "Authentication-Results", "Auto-Submitted", "Precedence", "List-Id", "List-Unsubscribe", "X-Auto-Response-Suppress",
)
SPECIAL_USE = frozenset({"\\All", "\\Archive", "\\Drafts", "\\Flagged", "\\Junk", "\\Sent", "\\Trash"})
SMTP_POLICY = policy.SMTP.clone(cte_type="7bit")
CHARSET_ALIASES = {"unknown-8bit": "utf-8", "x-unknown": "utf-8", "unknown": "utf-8", "default": "utf-8", "gb2312": "gb18030", "gbk": "gb18030", "ks_c_5601-1987": "cp949"}
LOCAL_PART_RE = re.compile(r"^[A-Za-z0-9!#$%&'*+/=?^_`{|}~-]+(?:\.[A-Za-z0-9!#$%&'*+/=?^_`{|}~-]+)*$")
DOMAIN_LABEL_RE = re.compile(r"^(?!-)[a-z0-9-]{1,63}(?<!-)$")
HOST_RE = re.compile(r"^(?=.{1,253}$)(?!-)[A-Za-z0-9-]{1,63}(?<!-)(?:\.(?!-)[A-Za-z0-9-]{1,63}(?<!-))*$")
MESSAGE_ID_RE = re.compile(r"<[^<>\s]{1,994}>")
MAILBOX_RE = re.compile(r"^[^\x00-\x1f\x7f]{1,512}$")
SEARCH_DATE_RE = re.compile(r"^\d{4}-\d{2}-\d{2}$")
REPLY_PREFIX_RE = re.compile(r"^\s*re\s*(?:\[\d+\])?\s*:", re.IGNORECASE)
FORWARD_PREFIX_RE = re.compile(r"^\s*(?:fwd?|fw)\s*(?:\[\d+\])?\s*:", re.IGNORECASE)
AUTH_RESULT_RE = re.compile(r"^\s*(dmarc|dkim|spf)\s*=\s*([A-Za-z]+)", re.IGNORECASE)
AUTH_PROPERTY_RE = re.compile(r"(header\.from|header\.d|header\.i|smtp\.mailfrom|smtp\.from)\s*=\s*(\"(?:[^\"\\]|\\.)*\"|[^\s;\"]+)", re.IGNORECASE)
GROUPED_APP_PASSWORD_RE = re.compile(r"^[A-Za-z]{4}(?:[ \u00a0][A-Za-z]{4}){3}$")
MAX_MIME_PARTS = 512
MAX_ADDRESS_HEADER_CHARS = 16384


def require_object(value, name):
    if not isinstance(value, dict):
        raise ContractError(f"{name} must be a JSON object")
    return value


def require_keys(value, required, optional, name):
    value = require_object(value, name)
    missing = sorted(set(required) - set(value))
    unknown = sorted(set(value) - set(required) - set(optional))
    if missing:
        raise ContractError(f"{name} is missing {', '.join(missing)}")
    if unknown:
        raise ContractError(f"{name} does not accept {', '.join(unknown)}")
    return value


def require_string(value, name, allow_empty=False, maximum=None):
    if not isinstance(value, str) or (not allow_empty and not value.strip()):
        raise ContractError(f"{name} must be a{' ' if allow_empty else ' non-empty '}string")
    if maximum is not None and len(value) > maximum:
        raise ContractError(f"{name} exceeds {maximum} characters")
    return value


def require_bool(value, name):
    if not isinstance(value, bool):
        raise ContractError(f"{name} must be true or false")
    return value


def require_int(value, name, minimum=None, maximum=None):
    if isinstance(value, bool) or not isinstance(value, int):
        raise ContractError(f"{name} must be an integer")
    if minimum is not None and value < minimum:
        raise ContractError(f"{name} must be at least {minimum}")
    if maximum is not None and value > maximum:
        raise ContractError(f"{name} must be at most {maximum}")
    return value


def require_string_list(value, name, allow_empty=True):
    if not isinstance(value, list) or (not allow_empty and not value):
        raise ContractError(f"{name} must be an array of strings")
    seen = set()
    for index, item in enumerate(value):
        require_string(item, f"{name}[{index}]")
        if item in seen:
            raise ContractError(f"{name} repeats {item!r}")
        seen.add(item)
    return value


def require_int_list(value, name):
    if not isinstance(value, list):
        raise ContractError(f"{name} must be an array of ports")
    for index, item in enumerate(value):
        require_int(item, f"{name}[{index}]", 1, 65535)
    return value


def validate_environment(value):
    environment = require_keys(value, set(ENVIRONMENT_KEYS), set(), "Email interface environment")
    names = [require_string(environment[key], f"Email interface environment.{key}") for key in ENVIRONMENT_KEYS]
    if len(set(names)) != len(names):
        raise ContractError("Email interface environment names must be distinct")
    return environment


def validate_host(value, name):
    value = require_string(value, name, maximum=255)
    if not HOST_RE.match(value):
        raise ContractError(f"{name} {value!r} is not a server name")
    return value


def validate_endpoint_spec(value, name):
    value = require_keys(value, {"host", "port", "tls"}, set(), name)
    validate_host(value["host"], f"{name}.host")
    require_int(value["port"], f"{name}.port", 1, 65535)
    if value["tls"] not in TLS_MODES:
        raise ContractError(f"{name}.tls must be implicit or starttls")
    return value


def validate_profile(value, name, required):
    value = require_keys(value, required | {"sent_copy", "authserv_ids", "sign_in_help"}, set(), name)
    if value["sent_copy"] not in SENT_COPY_MODES:
        raise ContractError(f"{name}.sent_copy must be provider, append or none")
    for index, pattern in enumerate(require_string_list(value["authserv_ids"], f"{name}.authserv_ids")):
        candidate = pattern[2:] if pattern.startswith("*.") else pattern
        if pattern != "(none)" and not HOST_RE.match(candidate):
            raise ContractError(f"{name}.authserv_ids[{index}] is not a server name pattern")
    require_string(value["sign_in_help"], f"{name}.sign_in_help", maximum=160)
    return value


def validate_config(config):
    require_keys(
        config,
        {"schema_version", "ledger", "providers", "custom_provider", "endpoints", "transport", "intake", "authz", "content", "outbound", "environment"},
        set(),
        "Email config",
    )
    if config["schema_version"] != 1:
        raise ContractError("Email config schema_version must be 1")
    validate_environment(config["environment"])
    ledger = require_keys(config["ledger"], {"max_row_bytes"}, set(), "Email ledger")
    require_int(ledger["max_row_bytes"], "Email ledger.max_row_bytes", 256, 16777216)
    if not isinstance(config["providers"], list):
        raise ContractError("Email providers must be an array")
    ids, domains = set(), set()
    for index, provider in enumerate(config["providers"]):
        name = f"Email providers[{index}]"
        validate_profile(provider, name, {"id", "domains", "imap", "smtp"})
        require_string(provider["id"], f"{name}.id", maximum=64)
        if provider["id"] in ids:
            raise ContractError(f"Email provider {provider['id']!r} is repeated")
        ids.add(provider["id"])
        for domain in require_string_list(provider["domains"], f"{name}.domains", allow_empty=False):
            if normalize_domain(domain) != domain or domain in domains:
                raise ContractError(f"{name}.domains has an invalid or repeated domain {domain!r}")
            domains.add(domain)
        validate_endpoint_spec(provider["imap"], f"{name}.imap")
        validate_endpoint_spec(provider["smtp"], f"{name}.smtp")
    validate_profile(config["custom_provider"], "Email custom_provider", set())
    endpoints = require_keys(config["endpoints"], {"imap", "smtp"}, set(), "Email endpoints")
    for protocol in ("imap", "smtp"):
        spec = require_keys(endpoints[protocol], {"default_port", "implicit_tls_ports", "starttls_ports", "discovery_ports"}, set(), f"Email endpoints.{protocol}")
        require_int(spec["default_port"], f"Email endpoints.{protocol}.default_port", 1, 65535)
        implicit = set(require_int_list(spec["implicit_tls_ports"], f"Email endpoints.{protocol}.implicit_tls_ports"))
        starttls = set(require_int_list(spec["starttls_ports"], f"Email endpoints.{protocol}.starttls_ports"))
        if implicit & starttls:
            raise ContractError(f"Email endpoints.{protocol} lists port {min(implicit & starttls)} for both TLS modes")
        discovery = require_int_list(spec["discovery_ports"], f"Email endpoints.{protocol}.discovery_ports")
        if not discovery or discovery[0] != spec["default_port"] or len(set(discovery)) != len(discovery):
            raise ContractError(f"Email endpoints.{protocol}.discovery_ports must start with the default port and not repeat")
    transport = require_keys(
        config["transport"],
        {"connect_timeout_ms", "command_timeout_ms", "idle_refresh_ms", "poll_interval_ms", "outbound_poll_ms", "reconnect_backoff_ms", "reconnect_backoff_cap_ms", "sign_in_retry_ms", "sign_in_retry_cap_ms"},
        set(),
        "Email transport",
    )
    for key, low, high in (
        ("connect_timeout_ms", 1000, 120000),
        ("command_timeout_ms", 1000, 600000),
        ("idle_refresh_ms", 60000, 1740000),
        ("poll_interval_ms", 5000, 3600000),
        ("outbound_poll_ms", 10, 10000),
        ("reconnect_backoff_ms", 100, 600000),
        ("reconnect_backoff_cap_ms", 100, 3600000),
        ("sign_in_retry_ms", 1000, 86400000),
        ("sign_in_retry_cap_ms", 1000, 86400000),
    ):
        require_int(transport[key], f"Email transport.{key}", low, high)
    if transport["reconnect_backoff_cap_ms"] < transport["reconnect_backoff_ms"] or transport["sign_in_retry_cap_ms"] < transport["sign_in_retry_ms"]:
        raise ContractError("Email transport backoff caps must not be below their initial delays")
    intake = require_keys(
        config["intake"],
        {"event_label", "mailboxes", "use_idle", "backlog_messages", "max_messages_per_cycle", "message_key_retention", "mark_seen"},
        set(),
        "Email intake",
    )
    require_string(intake["event_label"], "Email intake.event_label")
    for index, mailbox in enumerate(require_string_list(intake["mailboxes"], "Email intake.mailboxes", allow_empty=False)):
        require_mailbox_name(mailbox, f"Email intake.mailboxes[{index}]")
    if len(intake["mailboxes"]) > 8:
        raise ContractError("Email intake watches at most 8 mailboxes")
    require_bool(intake["use_idle"], "Email intake.use_idle")
    require_bool(intake["mark_seen"], "Email intake.mark_seen")
    require_int(intake["backlog_messages"], "Email intake.backlog_messages", 0, 500)
    require_int(intake["max_messages_per_cycle"], "Email intake.max_messages_per_cycle", 1, 1000)
    require_int(intake["message_key_retention"], "Email intake.message_key_retention", 100, 1000000)
    authz = require_keys(
        config["authz"],
        {"include_account_address", "allowed_senders", "allow_all_senders", "require_verified_sender", "ignore_automated", "automated_sender_fragments", "automated_header_presence", "automated_header_values"},
        set(),
        "Email authz",
    )
    for key in ("include_account_address", "allow_all_senders", "require_verified_sender", "ignore_automated"):
        require_bool(authz[key], f"Email authz.{key}")
    parse_allowed_senders(require_string_list(authz["allowed_senders"], "Email authz.allowed_senders"))
    require_string_list(authz["automated_sender_fragments"], "Email authz.automated_sender_fragments")
    require_string_list(authz["automated_header_presence"], "Email authz.automated_header_presence")
    for header, values in require_object(authz["automated_header_values"], "Email authz.automated_header_values").items():
        require_string_list(values, f"Email authz.automated_header_values.{header}", allow_empty=False)
    content = require_keys(
        config["content"],
        {"max_message_bytes", "max_header_bytes", "max_body_chars", "max_attachments", "max_attachment_bytes", "text_preview_bytes", "text_preview_media_types", "file_label", "max_images_per_event", "max_image_bytes_per_event"},
        set(),
        "Email content",
    )
    require_string(content["file_label"], "Email content.file_label")
    for key, low, high in (
        ("max_message_bytes", 65536, 268435456),
        ("max_header_bytes", 4096, 4194304),
        ("max_body_chars", 1000, 4000000),
        ("max_attachments", 0, 200),
        ("max_attachment_bytes", 1024, 268435456),
        ("text_preview_bytes", 0, 1048576),
        ("max_images_per_event", 0, 16),
        ("max_image_bytes_per_event", 0, 67108864),
    ):
        require_int(content[key], f"Email content.{key}", low, high)
    require_string_list(content["text_preview_media_types"], "Email content.text_preview_media_types")
    outbound = require_keys(
        config["outbound"],
        {
            "max_recipients", "max_subject_chars", "max_body_chars", "max_attachments",
            "max_total_attachment_bytes", "default_search_results", "max_search_results", "max_fetch_body_chars", "max_manifest_bytes",
            "max_payload_bytes", "auto_submitted", "from_name", "reference_limit",
        },
        set(),
        "Email outbound",
    )
    for key, low, high in (
        ("max_recipients", 1, 500),
        ("max_subject_chars", 1, 998),
        ("max_body_chars", 1000, 4000000),
        ("max_attachments", 0, 100),
        ("max_total_attachment_bytes", 0, 268435456),
        ("default_search_results", 1, 500),
        ("max_search_results", 1, 500),
        ("max_fetch_body_chars", 1000, 4000000),
        ("max_manifest_bytes", 1024, 16777216),
        ("max_payload_bytes", 1024, 1073741824),
        ("reference_limit", 2, 100),
    ):
        require_int(outbound[key], f"Email outbound.{key}", low, high)
    if outbound["default_search_results"] > outbound["max_search_results"]:
        raise ContractError("Email outbound.default_search_results exceeds max_search_results")
    for key in ("auto_submitted", "from_name"):
        if outbound[key] is not None:
            require_string(outbound[key], f"Email outbound.{key}", maximum=128)
            if "\r" in outbound[key] or "\n" in outbound[key]:
                raise ContractError(f"Email outbound.{key} must be one line")
    return config


def special_use_flags(flags):
    known = {flag.lower(): flag for flag in SPECIAL_USE}
    return sorted({known[flag.lower()] for flag in flags if flag.lower() in known})


def require_mailbox_name(value, name):
    value = require_string(value, name, maximum=512)
    if value.strip() != value or not MAILBOX_RE.match(value):
        raise ContractError(f"{name} is not a mailbox name")
    return value


def normalize_domain(domain):
    if not isinstance(domain, str) or not domain or any(character.isspace() for character in domain):
        raise ContractError(f"{domain!r} is not a mail domain")
    try:
        encoded = domain.rstrip(".").encode("idna").decode("ascii").lower()
    except UnicodeError as error:
        raise ContractError(f"{domain!r} is not a mail domain") from error
    labels = encoded.split(".")
    if len(labels) < 2 or len(encoded) > 253 or not all(DOMAIN_LABEL_RE.match(label) for label in labels):
        raise ContractError(f"{domain!r} is not a mail domain")
    return encoded


def normalize_address(value, name="Email address"):
    if not isinstance(value, str):
        raise ContractError(f"{name} must be a string")
    text = value.strip()
    if not text or len(text) > 320 or text.count("@") != 1 or any(character in text for character in "\r\n\x00 <>,;\"()[]\\"):
        raise ContractError(f"{name} {value!r} is not an email address")
    local, domain = text.split("@")
    if len(local) > 64 or not LOCAL_PART_RE.match(local):
        raise ContractError(f"{name} {value!r} is not an email address")
    return f"{local}@{normalize_domain(domain)}"


def address_domain(address):
    return address.rsplit("@", 1)[1].lower()


def parse_allowed_senders(entries):
    addresses, domains = [], set()
    seen = set()
    for entry in entries:
        if not isinstance(entry, str):
            raise ContractError("Allowed senders must be text")
        for token in re.split(r"[,;\s]+", entry.strip()):
            if not token:
                continue
            if token.startswith("@"):
                domains.add(normalize_domain(token[1:]))
                continue
            address = normalize_address(token, "Allowed sender")
            if address.lower() not in seen:
                seen.add(address.lower())
                addresses.append(address)
    return {"addresses": addresses, "domains": domains}


def parse_endpoint(value, spec, name):
    text = value.strip()
    if not text or any(character.isspace() for character in text) or "/" in text or "@" in text:
        raise ContractError(f"{name} must be a server name, optionally followed by :port")
    host, port = text, spec["default_port"]
    if text.count(":") == 1:
        host, port_text = text.split(":")
        if not port_text.isdigit():
            raise ContractError(f"{name} has an invalid port")
        port = int(port_text)
    elif ":" in text:
        raise ContractError(f"{name} must be a server name, optionally followed by :port")
    host = host.lower().rstrip(".")
    if not HOST_RE.match(host) or not 1 <= port <= 65535:
        raise ContractError(f"{name} {value!r} is not a server name")
    tls = "starttls" if port in spec["starttls_ports"] else "implicit"
    return {"host": host, "port": port, "tls": tls}


def find_provider(config, domain=None, imap_host=None):
    for provider in config["providers"]:
        if domain is not None and domain in provider["domains"]:
            return provider
    for provider in config["providers"]:
        if imap_host is not None and provider["imap"]["host"] == imap_host:
            return provider
    return None


def resolve_account(config, values):
    environment = config["environment"]
    raw_address = (values.get(environment["address_env"]) or "").strip()
    if not raw_address:
        raise ContractError("The email address is not configured; connect Email in Applications")
    address = normalize_address(raw_address)
    password = (values.get(environment["password_env"]) or "").strip()
    if GROUPED_APP_PASSWORD_RE.match(password):
        password = re.sub("[ \u00a0]", "", password)
    if not password:
        raise ContractError("The email app password is not configured; connect Email in Applications")
    domain = address_domain(address)
    imap_raw = (values.get(environment["imap_host_env"]) or "").strip()
    smtp_raw = (values.get(environment["smtp_host_env"]) or "").strip()
    imap = parse_endpoint(imap_raw, config["endpoints"]["imap"], "IMAP server") if imap_raw else None
    smtp = parse_endpoint(smtp_raw, config["endpoints"]["smtp"], "SMTP server") if smtp_raw else None
    provider = find_provider(config, domain=domain) or (find_provider(config, imap_host=imap["host"]) if imap else None)
    if imap is None and provider is not None:
        imap = dict(provider["imap"])
    if smtp is None and provider is not None:
        smtp = dict(provider["smtp"])
    if imap is None or smtp is None:
        raise ContractError(f"{domain} has no built-in mail server settings; enter its IMAP and SMTP servers")
    profile = provider or config["custom_provider"]
    allowed = parse_allowed_senders([values.get(environment["allowed_senders_env"]) or ""] + list(config["authz"]["allowed_senders"]))
    return {
        "address": address,
        "password": password,
        "domain": domain,
        "imap": imap,
        "smtp": smtp,
        "provider": provider["id"] if provider else None,
        "sent_copy": profile["sent_copy"],
        "authserv_ids": list(profile["authserv_ids"]),
        "sign_in_help": profile["sign_in_help"],
        "allowed": allowed,
    }


def default_recipient(account):
    return account["allowed"]["addresses"][0] if account["allowed"]["addresses"] else account["address"]


def safe_decode(payload, charset):
    label = (charset or "utf-8").strip().strip("\"'").lower() or "utf-8"
    for candidate in (CHARSET_ALIASES.get(label, label), "utf-8"):
        try:
            return payload.decode(candidate, errors="replace")
        except (LookupError, ValueError):
            continue
    return payload.decode("latin-1", errors="replace")


def unfold(value):
    return re.sub(r"\r?\n[ \t]+", " ", value).replace("\r", " ").replace("\n", " ")


def clean_text(text):
    if not re.search("[\ud800-\udfff]", text):
        return text
    try:
        return text.encode("utf-8", "surrogateescape").decode("utf-8", "replace")
    except UnicodeEncodeError:
        return text.encode("utf-8", "replace").decode("utf-8")


def decode_words(value, limit=None):
    if value is None:
        return ""
    if isinstance(value, str):
        value = unfold(clean_text(value))
    try:
        parts = decode_header(value)
    except Exception:
        parts = [(clean_text(str(value)), None)]
    decoded = unfold("".join(safe_decode(part, charset) if isinstance(part, bytes) else clean_text(part) for part, charset in parts)).strip()
    return decoded if limit is None else decoded[:limit]


def header_values(message, name):
    try:
        values = list(message.get_all(name, []))
    except Exception:
        return []
    return [unfold(clean_text(value)) if isinstance(value, str) else decode_words(value) for value in values]


def first_header(message, name, limit=None):
    values = header_values(message, name)
    return decode_words(values[0], limit) if values else ""


def parse_mailboxes(message, name, limit=1000):
    raw = [value for value in header_values(message, name) if len(value) <= MAX_ADDRESS_HEADER_CHARS]
    result, seen = [], set()
    for display_name, address in getaddresses(raw)[:limit]:
        try:
            normalized = normalize_address(address)
        except ContractError:
            continue
        if normalized.lower() in seen:
            continue
        seen.add(normalized.lower())
        result.append({"address": normalized, "name": decode_words(display_name, 256)})
    return result


def message_ids(value):
    return list(dict.fromkeys(MESSAGE_ID_RE.findall(value or "")))


def iso_date(value):
    if not value:
        return None
    try:
        parsed = parsedate_to_datetime(value)
    except (TypeError, ValueError, IndexError, OverflowError):
        return None
    return parsed.isoformat() if parsed is not None else None


def parse_header_block(raw_headers):
    return BytesHeaderParser(policy=policy.compat32).parsebytes(raw_headers)


def header_summary(message):
    return {
        "message_id": (message_ids(first_header(message, "Message-ID")) or [None])[0],
        "in_reply_to": message_ids(" ".join(header_values(message, "In-Reply-To"))),
        "references": message_ids(" ".join(header_values(message, "References"))),
        "date": iso_date(first_header(message, "Date")),
        "date_header": first_header(message, "Date", 256),
        "from": (parse_mailboxes(message, "From", 10) or [None])[0],
        "reply_to": parse_mailboxes(message, "Reply-To", 50),
        "to": parse_mailboxes(message, "To"),
        "cc": parse_mailboxes(message, "Cc"),
        "subject": first_header(message, "Subject", 998),
    }


def automated_reason(message, sender, authz):
    lowered = sender.lower()
    for fragment in authz["automated_sender_fragments"]:
        if fragment.lower() in lowered:
            return f"automated sender {fragment}"
    for header in authz["automated_header_presence"]:
        if header_values(message, header):
            return f"{header} header"
    for header, values in authz["automated_header_values"].items():
        for value in header_values(message, header):
            token = value.strip().lower().split(";", 1)[0].strip()
            if token in {item.lower() for item in values}:
                return f"{header}: {token}"
    return None


def split_auth_results(value):
    text = unfold(value)
    clauses, current, quoted, depth, index = [], [], False, 0, 0
    while index < len(text):
        character = text[index]
        if quoted:
            current.append(character)
            if character == "\\" and index + 1 < len(text):
                current.append(text[index + 1])
                index += 2
                continue
            if character == '"':
                quoted = False
        elif depth:
            if character == "\\":
                index += 2
                continue
            if character == "(":
                depth += 1
            elif character == ")":
                depth -= 1
                if not depth:
                    current.append(" ")
        elif character == "(":
            depth = 1
        elif character == ")":
            return None
        elif character == '"':
            quoted = True
            current.append(character)
        elif character == ";":
            clauses.append("".join(current).strip())
            current = []
        else:
            current.append(character)
        index += 1
    if quoted or depth:
        return None
    clauses.append("".join(current).strip())
    return clauses


def auth_results_head(clauses):
    first = clauses[0] if clauses else ""
    if re.match(r"^[A-Za-z0-9_.-]+\s*=", first):
        return "(none)", clauses
    return (first.split()[0].lower() if first.split() else ""), clauses[1:]


def auth_results(clauses):
    results = []
    for clause in clauses:
        match = AUTH_RESULT_RE.match(clause)
        if not match:
            continue
        properties = {}
        for key, value in AUTH_PROPERTY_RE.findall(clause[match.end():]):
            properties.setdefault(key.lower(), value.strip('"'))
        results.append((match.group(1).lower(), match.group(2).lower(), properties))
    return results


def trusted_auth_results(message, patterns):
    exact = {pattern.lower() for pattern in patterns if not pattern.startswith("*.") and pattern != "(none)"}
    suffixes = [pattern[1:].lower() for pattern in patterns if pattern.startswith("*.")]
    topmost_only = "(none)" in patterns
    trusted, seen_servers, seen_methods = [], set(), set()
    for index, raw in enumerate(header_values(message, "Authentication-Results")):
        clauses = split_auth_results(raw)
        if clauses is None:
            break
        authserv, rest = auth_results_head(clauses)
        listed = authserv in exact and authserv not in seen_servers
        topmost = index == 0 and ((authserv == "(none)" and topmost_only) or any(authserv.endswith(suffix) for suffix in suffixes))
        if not (listed or topmost):
            break
        seen_servers.add(authserv)
        results = [row for row in auth_results(rest) if row[0] not in seen_methods]
        seen_methods.update(row[0] for row in results)
        trusted.extend(results)
        if not listed:
            break
    return trusted


def domains_aligned(left, right):
    left = (left or "").strip().lower().rstrip(".")
    right = (right or "").strip().lower().rstrip(".")
    return bool(left and right) and (left == right or left.endswith("." + right) or right.endswith("." + left))


def property_domain(value):
    return value.rsplit("@", 1)[-1] if value else ""


def verify_sender(message, sender, patterns):
    if not patterns:
        return False, "the receiving server's sender checks are not configured for this mailbox"
    results = trusted_auth_results(message, patterns)
    if not results:
        return False, "no sender checks from the receiving server"
    domain = address_domain(sender)
    for method, verdict, properties in results:
        if method == "dmarc" and verdict == "pass" and domains_aligned(properties.get("header.from") or domain, domain):
            return True, "dmarc=pass"
    for method, verdict, properties in results:
        signer = properties.get("header.d") or property_domain(properties.get("header.i", ""))
        if method == "dkim" and verdict == "pass" and domains_aligned(signer, domain):
            return True, "dkim=pass"
    for method, verdict, properties in results:
        envelope = property_domain(properties.get("smtp.mailfrom") or properties.get("smtp.from") or "")
        if method == "spf" and verdict == "pass" and domains_aligned(envelope, domain):
            return True, "spf=pass"
    return False, "the receiving server's sender checks did not pass"


def admission(message, account, config, own_message_ids, labels=()):
    summary = header_summary(message)
    sender = summary["from"]
    if sender is None:
        return {"admit": False, "reason": "no readable sender", "summary": summary}
    if summary["message_id"] and summary["message_id"] in own_message_ids:
        return {"admit": False, "reason": "sent by this interface", "summary": summary}
    authz = config["authz"]
    address = sender["address"].lower()
    allowed = account["allowed"]
    if authz["include_account_address"] and address == account["address"].lower():
        allowed_as = "account"
    elif address in {item.lower() for item in allowed["addresses"]}:
        allowed_as = "allowed_sender"
    elif address_domain(address) in allowed["domains"]:
        allowed_as = "allowed_domain"
    elif authz["allow_all_senders"]:
        allowed_as = "any_sender"
    else:
        return {"admit": False, "reason": "sender is not allowed", "summary": summary}
    if authz["ignore_automated"]:
        reason = automated_reason(message, address, authz)
        if reason:
            return {"admit": False, "reason": reason, "summary": summary}
    if address == account["address"].lower() and "\\Sent" in labels:
        verified, evidence = True, "sent from this account"
    else:
        verified, evidence = verify_sender(message, address, account["authserv_ids"])
    if authz["require_verified_sender"] and not verified:
        return {"admit": False, "reason": "sender is not verified", "summary": summary}
    return {
        "admit": True,
        "summary": summary,
        "sender": {**sender, "allowed_as": allowed_as, "verified": verified, "verification": evidence},
        "trusted": allowed_as != "any_sender" and verified,
    }


class HtmlText(HTMLParser):
    SKIP = frozenset({"head", "script", "style", "title", "noscript", "template", "svg"})
    BREAK = frozenset({"address", "article", "aside", "center", "dd", "div", "dl", "dt", "fieldset", "figcaption", "figure", "footer", "form", "header", "main", "nav", "section", "tbody", "tfoot", "thead", "tr"})
    PARAGRAPH = frozenset({"p", "h1", "h2", "h3", "h4", "h5", "h6", "table", "ul", "ol"})

    def __init__(self):
        super().__init__(convert_charrefs=True)
        self.lines = []
        self.line = ""
        self.depth = 0
        self.pre = 0
        self.skip = 0
        self.lists = []
        self.links = []

    def flush(self, blank=False):
        text = self.line.rstrip()
        if text:
            self.lines.append("> " * self.depth + text)
        self.line = ""
        if blank and self.lines and self.lines[-1] != "":
            self.lines.append("")

    def write(self, text):
        if self.skip:
            return
        if self.pre:
            pieces = text.split("\n")
            for index, piece in enumerate(pieces):
                if index:
                    self.flush()
                self.line += piece
            return
        text = re.sub(r"\s+", " ", text.replace("\xa0", " "))
        if not self.line.strip():
            text = text.lstrip()
        self.line += text

    def handle_starttag(self, tag, attrs):
        tag = tag.lower()
        if tag in self.SKIP:
            self.skip += 1
            return
        if self.skip:
            return
        attributes = dict(attrs)
        if tag == "br":
            self.flush()
        elif tag in self.PARAGRAPH:
            self.flush(blank=True)
            if tag in {"ul", "ol"}:
                self.lists.append([tag, 0])
        elif tag in self.BREAK:
            self.flush()
        elif tag == "li":
            self.flush()
            if self.lists and self.lists[-1][0] == "ol":
                self.lists[-1][1] += 1
                self.line = f"{'   ' * (len(self.lists) - 1)}{self.lists[-1][1]}. "
            else:
                self.line = f"{'   ' * max(len(self.lists) - 1, 0)}- "
        elif tag == "blockquote":
            self.flush(blank=True)
            self.depth += 1
        elif tag == "pre":
            self.flush(blank=True)
            self.pre += 1
        elif tag in {"td", "th"}:
            if self.line.strip():
                self.line += "  "
        elif tag == "hr":
            self.flush()
            self.lines.append("---")
        elif tag == "a":
            self.links.append((attributes.get("href") or "", len(self.line), len(self.lines)))

    def handle_endtag(self, tag):
        tag = tag.lower()
        if tag in self.SKIP:
            self.skip = max(self.skip - 1, 0)
            return
        if self.skip:
            return
        if tag in self.PARAGRAPH:
            if tag in {"ul", "ol"} and self.lists:
                self.lists.pop()
            self.flush(blank=True)
        elif tag in self.BREAK or tag == "li":
            self.flush()
        elif tag == "blockquote":
            self.flush(blank=True)
            self.depth = max(self.depth - 1, 0)
        elif tag == "pre":
            self.flush(blank=True)
            self.pre = max(self.pre - 1, 0)
        elif tag == "a" and self.links:
            href, start, line_count = self.links.pop()
            label = self.line[start:].strip() if line_count == len(self.lines) else ""
            target = href[7:] if href.lower().startswith("mailto:") else href
            if href.lower().startswith(("http://", "https://", "mailto:")) and len(href) <= 300 and target and target != label:
                self.line += f" ({target})" if label else target

    def handle_data(self, data):
        self.write(data)

    def text(self):
        self.flush()
        text = "\n".join(self.lines)
        return re.sub(r"\n{3,}", "\n\n", text).strip()


def html_to_text(value):
    parser = HtmlText()
    try:
        parser.feed(value)
        parser.close()
        return parser.text()
    except Exception:
        return re.sub(r"\s+", " ", html.unescape(re.sub(r"<[^>]+>", " ", value))).strip()


def part_disposition(part):
    try:
        return (part.get_content_disposition() or "").lower()
    except Exception:
        return ""


def part_filename(part):
    try:
        name = part.get_filename()
    except Exception:
        name = None
    return decode_words(clean_text(name), 255) if name else ""


def part_bytes(part):
    if part.get_content_type() == "message/rfc822" and isinstance(part.get_payload(), list) and part.get_payload():
        return part.get_payload(0).as_bytes(policy=policy.compat32)
    payload = part.get_payload(decode=True)
    if payload is None:
        raw = part.get_payload()
        payload = raw.encode("utf-8", errors="replace") if isinstance(raw, str) else b""
    return payload


def leaf_parts(message):
    stack, count = [message], 0
    while stack:
        part = stack.pop(0)
        count += 1
        if count > MAX_MIME_PARTS:
            raise ContractError("the message has too many MIME parts")
        if part.get_content_type() == "message/rfc822" or not part.is_multipart():
            yield part
            continue
        payload = part.get_payload()
        stack[0:0] = [child for child in payload if hasattr(child, "get_content_type")] if isinstance(payload, list) else []


def safe_filename(value, media_type):
    name = (value or "").replace("\\", "/").split("/")[-1]
    name = re.sub(r"[\x00-\x1f\x7f]", "", name).strip().lstrip(".").strip()
    if not name:
        name = "attachment" + (mimetypes.guess_extension(media_type or "") or ".bin")
    stem, dot, suffix = name.rpartition(".")
    if not dot:
        stem, suffix = name, ""
    suffix = suffix[:16]
    stem = stem[: 180 - len(suffix)]
    return f"{stem}.{suffix}" if suffix else stem


def media_type_matches(media_type, patterns):
    return any(media_type == pattern or (pattern.endswith("/*") and media_type.startswith(pattern[:-1])) for pattern in patterns)


def parse_message(raw, content, max_body_chars, store_attachment=None):
    message = BytesParser(policy=policy.compat32).parsebytes(raw)
    summary = header_summary(message)
    plain, rich, attachments, media, inline_parts, omitted = [], [], [], [], 0, 0
    for part in leaf_parts(message):
        media_type = part.get_content_type().lower()
        disposition = part_disposition(part)
        filename = part_filename(part)
        is_attachment = bool(filename) or disposition == "attachment" or media_type == "message/rfc822"
        if not is_attachment and media_type in {"text/plain", "text/html"}:
            text = safe_decode(part_bytes(part), part.get_content_charset())
            (plain if media_type == "text/plain" else rich).append(text)
            continue
        if not is_attachment:
            inline_parts += 1
            continue
        if len(attachments) >= content["max_attachments"]:
            omitted += 1
            continue
        attachments.append(attachment_row(part, media_type, filename, content, store_attachment, media))
    body_text = "\n\n".join(text.strip() for text in plain if text.strip())
    source = "text/plain" if body_text else "none"
    if not body_text and rich:
        body_text = html_to_text("\n".join(rich))
        source = "text/html" if body_text else "none"
    body = {"text": body_text[:max_body_chars], "source": source, "truncated": len(body_text) > max_body_chars}
    return {**summary, "body": body, "attachments": attachments, "media": media, "omitted_attachments": omitted, "inline_parts": inline_parts}


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


def attachment_row(part, media_type, filename, content, store_attachment, media):
    name = safe_filename(filename or ("message.eml" if media_type == "message/rfc822" else ""), media_type)
    data = part_bytes(part)
    row = {"filename": name, "media_type": media_type, "size": len(data), "sha256": hashlib.sha256(data).hexdigest()}
    if len(data) > content["max_attachment_bytes"]:
        row["omitted"] = f"larger than {content['max_attachment_bytes']} bytes"
        return row
    if store_attachment is not None:
        row["path"] = store_attachment(data, name, row["sha256"])
        embed_image(media, row, row["path"], data, content)
    if content["text_preview_bytes"] and len(data) <= content["text_preview_bytes"] and media_type_matches(media_type, content["text_preview_media_types"]):
        row["text_preview"] = safe_decode(data, part.get_content_charset())
    return row


def original_attachments(raw, limit_bytes):
    message = BytesParser(policy=policy.compat32).parsebytes(raw)
    result, total = [], 0
    for part in leaf_parts(message):
        media_type = part.get_content_type().lower()
        filename = part_filename(part)
        if not (filename or part_disposition(part) == "attachment" or media_type == "message/rfc822"):
            continue
        data = part_bytes(part)
        total += len(data)
        if total > limit_bytes:
            raise ContractError("the original attachments exceed the outbound attachment limit")
        result.append({"filename": safe_filename(filename or "message.eml", media_type), "media_type": media_type, "data": data})
    return result


def inbound_event(account, ref, decision, parsed, flags, received_at):
    message = {
        "ref": ref,
        "message_id": parsed["message_id"],
        "in_reply_to": parsed["in_reply_to"],
        "references": parsed["references"],
        "date": parsed["date"],
        "received_at": received_at,
        "from": parsed["from"],
        "reply_to": parsed["reply_to"],
        "to": parsed["to"],
        "cc": parsed["cc"],
        "subject": parsed["subject"],
        "body": parsed["body"],
        "attachments": parsed["attachments"],
        "omitted_attachments": parsed["omitted_attachments"],
        "inline_parts": parsed["inline_parts"],
        "flags": flags,
    }
    event = {"schema_version": 1, "event": "message.received", "account": account["address"], "trusted": decision["trusted"], "sender": decision["sender"], "message": message}
    if parsed.get("media"):
        event["media"] = parsed["media"]
    return event


def require_ref(value, name="ref"):
    ref = require_keys(value, {"mailbox", "uidvalidity", "uid"}, set(), f"Email {name}")
    require_mailbox_name(ref["mailbox"], f"Email {name}.mailbox")
    require_int(ref["uidvalidity"], f"Email {name}.uidvalidity", 1, 4294967295)
    require_int(ref["uid"], f"Email {name}.uid", 1, 4294967295)
    return ref


def require_message_id(value):
    value = require_string(value, "Email message_id", maximum=996).strip()
    if not MESSAGE_ID_RE.fullmatch(value):
        raise ContractError("Email message_id must look like <id@domain>")
    return value


def require_target(args, operation):
    if ("ref" in args) == ("message_id" in args):
        raise ContractError(f"{operation} needs exactly one of ref or message_id")
    return ("ref", require_ref(args["ref"])) if "ref" in args else ("message_id", require_message_id(args["message_id"]))


def require_refs(args, operation):
    if ("ref" in args) == ("refs" in args):
        raise ContractError(f"{operation} needs exactly one of ref or refs")
    refs = [args["ref"]] if "ref" in args else args["refs"]
    if not isinstance(refs, list) or not refs or len(refs) > 500:
        raise ContractError(f"{operation} refs must hold 1 to 500 message refs")
    refs = [require_ref(ref, f"refs[{index}]") for index, ref in enumerate(refs)]
    if len({(ref["mailbox"], ref["uidvalidity"]) for ref in refs}) != 1:
        raise ContractError(f"{operation} refs must come from one mailbox")
    return refs


def require_recipients(value, name, limit):
    entries = [value] if isinstance(value, str) else value
    if not isinstance(entries, list) or any(not isinstance(entry, str) for entry in entries):
        raise ContractError(f"{name} must be an address or an array of addresses")
    result = []
    for entry in entries:
        if any(character in entry for character in "\r\n\x00") or len(entry) > 1024:
            raise ContractError(f"{name} contains an invalid address")
        parsed = getaddresses([entry])
        if len(parsed) != 1:
            raise ContractError(f"{name} entry {entry!r} must name exactly one address")
        display, address = parsed[0]
        result.append({"address": normalize_address(address, name), "name": display.strip()[:128]})
    if len(result) > limit:
        raise ContractError(f"{name} exceeds {limit} recipients")
    return result


def require_attachment_specs(value, limit):
    if not isinstance(value, list) or len(value) > limit:
        raise ContractError(f"attachments must be an array of at most {limit} items")
    specs = []
    for index, item in enumerate(value):
        name = f"attachments[{index}]"
        item = require_object(item, name)
        carriers = [key for key in ("text", "content_base64", "artifact", "path") if key in item]
        if len(carriers) != 1:
            raise ContractError(f"{name} needs exactly one of text, content_base64, artifact or path")
        require_keys(item, {carriers[0]}, {"filename", "media_type"}, name)
        if "filename" in item:
            require_string(item["filename"], f"{name}.filename", maximum=255)
        if "media_type" in item:
            media_type = require_string(item["media_type"], f"{name}.media_type", maximum=127)
            if not re.fullmatch(r"[A-Za-z0-9!#$&^_.+-]+/[A-Za-z0-9!#$&^_.+-]+", media_type):
                raise ContractError(f"{name}.media_type is not a media type")
        if carriers[0] in {"content_base64", "artifact", "path"}:
            require_string(item[carriers[0]], f"{name}.{carriers[0]}")
        elif not isinstance(item["text"], str):
            raise ContractError(f"{name}.text must be a string")
        if carriers[0] in {"text", "content_base64"} and "filename" not in item:
            raise ContractError(f"{name} needs a filename")
        specs.append(item)
    return specs


def validate_handlers(catalog):
    for identity, spec in catalog["operations"].items():
        handler = require_keys(spec["handler_config"], {"operation", "read_only"}, set(), f"Email operation {identity} handler_config")
        if handler["operation"] not in OPERATIONS:
            raise ContractError(f"Email operation {identity} names unknown handler {handler['operation']!r}")
        require_bool(handler["read_only"], f"Email operation {identity} handler_config.read_only")
    return catalog


def normalize_arguments(operation, args, config):
    outbound = config["outbound"]
    if operation in SENDING_OPERATIONS:
        for key in ("to", "cc", "bcc"):
            if key in args:
                args[key] = require_recipients(args[key], key, outbound["max_recipients"])
        for key in ("text", "html"):
            if key in args:
                require_string(args[key], key, allow_empty=key == "text", maximum=outbound["max_body_chars"])
        if "subject" in args:
            subject = require_string(args["subject"], "subject", maximum=outbound["max_subject_chars"])
            if "\r" in subject or "\n" in subject:
                raise ContractError("subject must be one line")
        args["attachments"] = require_attachment_specs(args.get("attachments", []), outbound["max_attachments"])
        if operation == "message.send" and not args.get("to") and (args.get("cc") or args.get("bcc")):
            raise ContractError("send_message with cc or bcc must also name to")
    if "mailbox" in args:
        require_mailbox_name(args["mailbox"], "mailbox")
    if operation in {"message.reply", "message.forward", "message.fetch"}:
        args["target"] = require_target(args, operation)
    if operation in {"message.mark", "message.move"}:
        args["refs"] = require_refs(args, operation)
        args.pop("ref", None)
    if operation == "message.mark" and "seen" not in args and "flagged" not in args:
        raise ContractError("mark_messages needs seen, flagged or both")
    if operation == "message.move":
        require_mailbox_name(args["to_mailbox"], "to_mailbox")
    if operation == "message.search":
        validate_search(args, outbound)
    return args


def validate_search(args, outbound):
    for key in ("from", "to", "subject", "text"):
        if key in args:
            value = require_string(args[key], key, maximum=256)
            if any(character in value for character in "\r\n\x00"):
                raise ContractError(f"{key} must be one line")
    for key in ("since", "before"):
        if key in args:
            if not SEARCH_DATE_RE.match(require_string(args[key], key)):
                raise ContractError(f"{key} must be YYYY-MM-DD")
            try:
                datetime.date.fromisoformat(args[key])
            except ValueError as error:
                raise ContractError(f"{key} must be a real date") from error
    if "limit" in args:
        require_int(args["limit"], "limit", 1, outbound["max_search_results"])


def search_criteria(args):
    criteria = []
    for key, atom in (("from", "FROM"), ("to", "TO"), ("subject", "SUBJECT"), ("text", "TEXT")):
        if key in args:
            criteria.append((atom, args[key]))
    for key, atom in (("since", "SINCE"), ("before", "BEFORE")):
        if key in args:
            criteria.append((atom, datetime.date.fromisoformat(args[key]).strftime("%d-%b-%Y")))
    if "unseen" in args:
        criteria.append(("UNSEEN" if args["unseen"] else "SEEN", None))
    if "flagged" in args:
        criteria.append(("FLAGGED" if args["flagged"] else "UNFLAGGED", None))
    return criteria


def formatted_address(entry):
    return f'{entry["name"]} <{entry["address"]}>' if entry.get("name") else entry["address"]


def header_address(entry):
    return Address(display_name=entry.get("name") or "", addr_spec=entry["address"])


def reply_subject(subject):
    return subject if REPLY_PREFIX_RE.match(subject or "") else f"Re: {subject or ''}".rstrip()


def forward_subject(subject):
    return subject if FORWARD_PREFIX_RE.match(subject or "") else f"Fwd: {subject or ''}".rstrip()


def reference_chain(original, limit):
    chain = list(original.get("references") or [])
    if not chain and original.get("in_reply_to"):
        chain = list(original["in_reply_to"][:1])
    if original.get("message_id"):
        chain.append(original["message_id"])
    chain = list(dict.fromkeys(chain))
    return chain if len(chain) <= limit else chain[:1] + chain[-(limit - 1):]


def quoted_original(original):
    author = original["from"]
    who = f"{author['name']} <{author['address']}>" if author and author.get("name") else (author["address"] if author else "the sender")
    when = original.get("date_header") or "an earlier message"
    lines = original["body"]["text"].splitlines() or [""]
    return f"On {when}, {who} wrote:\n" + "\n".join(f"> {line}" if line else ">" for line in lines)


def forwarded_original(original):
    author = original["from"]
    header = [
        "---------- Forwarded message ----------",
        f"From: {formatted_address(author) if author else ''}",
        f"Date: {original.get('date_header') or ''}",
        f"Subject: {original.get('subject') or ''}",
        "To: " + ", ".join(formatted_address(item) for item in original.get("to", [])),
    ]
    return "\n".join(header) + "\n\n" + original["body"]["text"]


def without(entries, excluded):
    seen, result = {item.lower() for item in excluded}, []
    for entry in entries:
        if entry["address"].lower() not in seen:
            seen.add(entry["address"].lower())
            result.append(entry)
    return result


def plan_recipients(operation, args, account, original):
    own = account["address"]
    if operation == "message.send":
        to = args.get("to") or [{"address": default_recipient(account), "name": ""}]
        cc = args.get("cc", [])
    elif operation == "message.reply":
        author = original["from"]
        if author and author["address"].lower() == own.lower() and not original["reply_to"] and original["to"]:
            to = list(original["to"])
        else:
            to = original["reply_to"] or ([author] if author else [])
        if not to:
            raise ContractError("the original message has no sender to reply to")
        cc = list(args.get("cc", []))
        if args.get("reply_all"):
            cc = without(original["to"] + original["cc"] + cc, [own] + [item["address"] for item in to])
    else:
        to, cc = args["to"], args.get("cc", [])
    bcc = args.get("bcc", [])
    cc = without(cc, [item["address"] for item in to])
    bcc = without(bcc, [item["address"] for item in to + cc])
    return to, cc, bcc


def compose_message(operation, args, account, outbound, original, attachments, message_id, now):
    to, cc, bcc = plan_recipients(operation, args, account, original)
    if len(to) + len(cc) + len(bcc) > outbound["max_recipients"]:
        raise ContractError(f"the message exceeds {outbound['max_recipients']} recipients")
    text = args.get("text", "")
    html_body = args.get("html")
    references = []
    if operation == "message.send":
        subject = args["subject"]
    elif operation == "message.reply":
        subject = reply_subject(original["subject"])
        references = reference_chain(original, outbound["reference_limit"]) if original.get("message_id") else []
        if args.get("quote"):
            quote = quoted_original(original)
            text = f"{text.rstrip()}\n\n{quote}" if text.strip() else quote
            if html_body is not None:
                html_body = f"{html_body}<br><br><blockquote>{html.escape(original['body']['text']).replace(chr(10), '<br>')}</blockquote>"
    else:
        subject = forward_subject(original["subject"])
        forwarded = forwarded_original(original)
        text = f"{text.rstrip()}\n\n{forwarded}" if text.strip() else forwarded
        if html_body is not None:
            html_body = f"{html_body}<br><br><pre>{html.escape(forwarded)}</pre>"
    if len(text) > outbound["max_body_chars"]:
        raise ContractError(f"the message body exceeds {outbound['max_body_chars']} characters")
    message = EmailMessage(policy=SMTP_POLICY)
    message["From"] = header_address({"address": account["address"], "name": outbound["from_name"] or ""})
    message["To"] = [header_address(item) for item in to]
    if cc:
        message["Cc"] = [header_address(item) for item in cc]
    message["Subject"] = subject
    message["Date"] = format_datetime(now)
    message["Message-ID"] = message_id
    if references:
        message["In-Reply-To"] = references[-1]
        message["References"] = " ".join(references)
    if outbound["auto_submitted"]:
        message["Auto-Submitted"] = outbound["auto_submitted"]
    message.set_content(text or "\n")
    if html_body is not None:
        message.add_alternative(html_body, subtype="html")
    for attachment in attachments:
        if attachment["media_type"] == "message/rfc822":
            message.add_attachment(attachment["data"], maintype="application", subtype="octet-stream", filename=attachment["filename"])
            continue
        maintype, subtype = attachment["media_type"].split("/", 1)
        message.add_attachment(attachment["data"], maintype=maintype, subtype=subtype, filename=attachment["filename"])
    summary = {
        "message_id": message_id,
        "subject": subject,
        "to": [item["address"] for item in to],
        "cc": [item["address"] for item in cc],
        "bcc": [item["address"] for item in bcc],
        "attachments": [{"filename": item["filename"], "media_type": item["media_type"], "size": len(item["data"])} for item in attachments],
    }
    if references:
        summary["in_reply_to"] = references[-1]
    return message, [item["address"] for item in to + cc + bcc], summary


def serialize_message(message):
    return message.as_bytes(policy=SMTP_POLICY)


def operation_message_id(interface_id, source_seq, source_hash, domain):
    digest = hashlib.sha256(f"{interface_id}\0{source_seq}\0{source_hash}".encode("utf-8")).hexdigest()
    return f"<{digest[:32]}@{domain}>"


def inline_attachment(item, index):
    filename = safe_filename(item["filename"], item.get("media_type", ""))
    if "text" in item:
        data = item["text"].encode("utf-8")
        media_type = item.get("media_type") or mimetypes.guess_type(filename)[0] or "text/plain"
    else:
        try:
            data = base64.b64decode(item["content_base64"], validate=True)
        except (binascii.Error, ValueError) as error:
            raise ContractError(f"attachments[{index}].content_base64 is not base64") from error
        media_type = item.get("media_type") or mimetypes.guess_type(filename)[0] or "application/octet-stream"
    return {"filename": filename, "media_type": media_type.lower(), "data": data}
