import base64
import datetime
import imaplib
import re
import select
import smtplib
import socket
import ssl
import time


MAX_LINE = 1000000
CHANGE_RE = re.compile(rb"^\* \d+ (?:EXISTS|RECENT)\b", re.IGNORECASE)
LITERAL_RE = re.compile(rb"\{(\d+)\+?\}\r?\n$")
RESPONSE_START_RE = re.compile(rb"^\d+ \(")
LIST_RE = re.compile(rb'^\((?P<flags>[^)]*)\)\s+(?P<delimiter>NIL|"(?:[^"\\]|\\.)*")\s*(?P<name>.*)$', re.IGNORECASE)
TEMPORARY_RE = re.compile(r"\[(?:UNAVAILABLE|INUSE|LIMIT|SERVERBUG)\]|try again|temporar|too many (?:simultaneous|connections)", re.IGNORECASE)


class MailError(RuntimeError):
    def __init__(self, kind, message, retryable=False, server_text=""):
        super().__init__(message)
        self.kind = kind
        self.retryable = retryable
        self.server_text = server_text


class CommitUncertain(RuntimeError):
    pass


def tls_context():
    context = ssl.create_default_context()
    context.minimum_version = ssl.TLSVersion.TLSv1_2
    return context


def server_text(value, limit=300):
    if isinstance(value, (list, tuple)):
        value = b" ".join(item if isinstance(item, bytes) else str(item).encode("utf-8", "replace") for item in value if item is not None)
    if isinstance(value, bytes):
        value = value.decode("utf-8", "replace")
    return re.sub(r"\s+", " ", str(value or "")).strip()[:limit]


def connection_error(endpoint, error):
    where = f"{endpoint['host']}:{endpoint['port']}"
    if isinstance(error, MailError):
        return error
    if isinstance(error, ssl.SSLCertVerificationError):
        return MailError("tls", f"The certificate of {endpoint['host']} is not trusted ({error.verify_message or 'verification failed'})")
    if isinstance(error, ssl.SSLError):
        return MailError("tls", f"The encrypted connection to {where} failed", retryable=True, server_text=server_text(str(error)))
    if isinstance(error, socket.gaierror):
        return MailError("transport", f"The server name {endpoint['host']} could not be found", retryable=True)
    if isinstance(error, socket.timeout):
        return MailError("transport", f"{where} did not answer in time", retryable=True)
    if isinstance(error, ConnectionRefusedError):
        return MailError("transport", f"{where} refused the connection", retryable=True)
    return MailError("transport", f"The connection to {where} failed", retryable=True, server_text=server_text(str(error)))


def encode_mailbox(value):
    output, pending = [], []

    def flush():
        if pending:
            encoded = base64.b64encode("".join(pending).encode("utf-16-be")).decode("ascii").rstrip("=").replace("/", ",")
            output.append(f"&{encoded}-")
            pending.clear()

    for character in value:
        if 0x20 <= ord(character) <= 0x7E:
            flush()
            output.append("&-" if character == "&" else character)
        else:
            pending.append(character)
    flush()
    return "".join(output)


def decode_mailbox(value):
    output, index = [], 0
    while index < len(value):
        if value[index] != "&":
            output.append(value[index])
            index += 1
            continue
        end = value.find("-", index)
        if end < 0:
            return value
        chunk = value[index + 1:end]
        if not chunk:
            output.append("&")
        else:
            padded = chunk.replace(",", "/") + "=" * (-len(chunk) % 4)
            try:
                output.append(base64.b64decode(padded).decode("utf-16-be"))
            except (ValueError, UnicodeDecodeError):
                return value
        index = end + 1
    return "".join(output)


def quote(value):
    return '"' + value.replace("\\", "\\\\").replace('"', '\\"') + '"'


def quote_mailbox(name):
    return quote(encode_mailbox(name))


def uid_set(uids):
    return ",".join(str(uid) for uid in sorted(set(uids)))


def parse_uids(data):
    values = []
    for item in data or []:
        if isinstance(item, bytes):
            values.extend(int(token) for token in item.split() if token.isdigit())
    return values


class LineReader:
    def __init__(self, sock):
        self.sock = sock
        self.buffer = bytearray()

    def has_data(self):
        if self.buffer:
            return True
        pending = getattr(self.sock, "pending", None)
        return bool(pending and pending())

    def fill(self):
        chunk = self.sock.recv(65536)
        if not chunk:
            return False
        self.buffer.extend(chunk)
        return True

    def take(self, count):
        data = bytes(self.buffer[:count])
        del self.buffer[:count]
        return data

    def readline(self, limit):
        while True:
            index = self.buffer.find(b"\n")
            if index >= 0:
                return self.take(index + 1)
            if len(self.buffer) > limit:
                return self.take(limit + 1)
            if not self.fill():
                return self.take(len(self.buffer))

    def read(self, size):
        while len(self.buffer) < size and self.fill():
            pass
        return self.take(min(size, len(self.buffer)))


class ReaderMixin:
    def open(self, host="", port=None, timeout=None):
        super().open(host, port, timeout)
        self.octopus_reader = LineReader(self.sock)

    def read(self, size):
        return self.octopus_reader.read(size)

    def readline(self):
        line = self.octopus_reader.readline(MAX_LINE)
        if len(line) > MAX_LINE:
            raise self.error(f"got more than {MAX_LINE} bytes")
        return line


class TlsImap(ReaderMixin, imaplib.IMAP4_SSL):
    pass


class PlainImap(ReaderMixin, imaplib.IMAP4):
    def start_tls(self, context):
        if "STARTTLS" not in self.capabilities:
            raise MailError("tls", f"{self.host} does not offer STARTTLS on port {self.port}; Octopus never signs in without encryption")
        kind, data = self._simple_command("STARTTLS")
        if kind != "OK":
            raise MailError("tls", f"{self.host} refused to start TLS", server_text=server_text(data))
        if self.octopus_reader.buffer:
            raise MailError("tls", f"{self.host} sent data before TLS started")
        self.sock = context.wrap_socket(self.sock, server_hostname=self.host)
        self.octopus_reader = LineReader(self.sock)
        self._get_capabilities()


class FetchResponse:
    def __init__(self):
        self.meta = b""
        self.literals = []

    def number(self, name):
        match = re.search(rb"\b" + re.escape(name) + rb" (\d+)", self.meta, re.IGNORECASE)
        return int(match.group(1)) if match else None

    def flags(self):
        match = re.search(rb"\bFLAGS \(([^)]*)\)", self.meta, re.IGNORECASE)
        return sorted(token.decode("utf-8", "replace") for token in match.group(1).split()) if match else []

    def internal_date(self):
        match = re.search(rb'INTERNALDATE "([^"]+)"', self.meta, re.IGNORECASE)
        if not match:
            return None
        try:
            return datetime.datetime.strptime(match.group(1).decode("ascii").strip(), "%d-%b-%Y %H:%M:%S %z").isoformat()
        except ValueError:
            return None

    def labels(self):
        match = re.search(rb'X-GM-LABELS \(((?:[^()"]|"(?:[^"\\]|\\.)*")*)\)', self.meta, re.IGNORECASE)
        if not match:
            return []
        labels = []
        for quoted, atom in re.findall(rb'"((?:[^"\\]|\\.)*)"|([^\s"]+)', match.group(1)):
            value = re.sub(rb"\\(.)", rb"\1", quoted) if quoted else atom
            labels.append(value.decode("utf-8", "replace"))
        return labels

    def body(self):
        for prefix, literal in self.literals:
            if re.search(rb"BODY\[[^\]]*\](?:<\d+>)?\s*\{\d+\}$", prefix, re.IGNORECASE):
                return literal
        return None


def fetch_responses(data):
    responses = []
    for item in data or []:
        prefix = item[0] if isinstance(item, tuple) else item
        if not isinstance(prefix, bytes):
            continue
        if RESPONSE_START_RE.match(prefix) or not responses:
            responses.append(FetchResponse())
        responses[-1].meta += b" " + prefix
        if isinstance(item, tuple) and len(item) > 1 and isinstance(item[1], bytes):
            responses[-1].literals.append((prefix.rstrip(), item[1]))
    return responses


class ImapSession:
    def __init__(self, imap, endpoint):
        self.imap = imap
        self.endpoint = endpoint
        self.capabilities = set()
        self.selected = None

    @classmethod
    def open(cls, endpoint, address, password, connect_timeout, command_timeout):
        try:
            if endpoint["tls"] == "implicit":
                imap = TlsImap(endpoint["host"], endpoint["port"], ssl_context=tls_context(), timeout=connect_timeout)
            else:
                imap = PlainImap(endpoint["host"], endpoint["port"], timeout=connect_timeout)
        except (MailError, OSError, imaplib.IMAP4.error) as error:
            raise connection_error(endpoint, error) from None
        session = cls(imap, endpoint)
        try:
            if endpoint["tls"] == "starttls":
                imap.start_tls(tls_context())
            imap.sock.settimeout(command_timeout)
            imap.sock.setsockopt(socket.SOL_SOCKET, socket.SO_KEEPALIVE, 1)
            session.sign_in(address, password)
            session.refresh_capabilities()
            session.identify()
        except BaseException as error:
            session.close()
            if isinstance(error, (OSError, imaplib.IMAP4.error)):
                raise connection_error(endpoint, error) from None
            raise
        return session

    def sign_in(self, address, password):
        capabilities = {item.upper() for item in self.imap.capabilities}
        try:
            if "AUTH=PLAIN" in capabilities:
                payload = b"\0" + address.encode("utf-8") + b"\0" + password.encode("utf-8")
                self.imap.authenticate("PLAIN", lambda _challenge: payload)
            elif "LOGINDISABLED" in capabilities:
                raise MailError("authentication", f"{self.endpoint['host']} does not accept password sign-in on this connection")
            else:
                self.imap.login(address, password)
        except imaplib.IMAP4.abort as error:
            raise MailError("transport", f"{self.endpoint['host']} closed the connection during sign-in", retryable=True, server_text=server_text(str(error))) from None
        except imaplib.IMAP4.error as error:
            text = server_text(str(error))
            temporary = bool(TEMPORARY_RE.search(text))
            raise MailError("transport" if temporary else "authentication", f"{self.endpoint['host']} rejected the sign-in", retryable=temporary, server_text=text) from None
        except UnicodeEncodeError:
            raise MailError("authentication", f"{self.endpoint['host']} accepts only plain ASCII passwords") from None

    def refresh_capabilities(self):
        kind, data = self.imap.capability()
        if kind == "OK" and data and isinstance(data[0], bytes):
            self.capabilities = {token.upper() for token in data[0].decode("ascii", "replace").split()}
        else:
            self.capabilities = {item.upper() for item in self.imap.capabilities}

    def identify(self):
        if "ID" not in self.capabilities:
            return
        try:
            self.imap.xatom("ID", '("name" "Octopus")')
        except imaplib.IMAP4.error:
            pass

    def has(self, capability):
        return capability.upper() in self.capabilities

    def call(self, method, *args):
        try:
            kind, data = getattr(self.imap, method)(*args)
        except imaplib.IMAP4.abort as error:
            raise MailError("transport", f"{self.endpoint['host']} closed the mailbox connection", retryable=True, server_text=server_text(str(error))) from None
        except imaplib.IMAP4.error as error:
            raise MailError("protocol", f"{self.endpoint['host']} rejected {method.upper()}", server_text=server_text(str(error))) from None
        except (OSError, socket.timeout) as error:
            raise connection_error(self.endpoint, error) from None
        if kind != "OK":
            raise MailError("protocol", f"{self.endpoint['host']} answered {kind} to {method.upper()}", server_text=server_text(data))
        return data

    def uid(self, command, *args):
        try:
            kind, data = self.imap.uid(command, *args)
        except imaplib.IMAP4.abort as error:
            raise MailError("transport", f"{self.endpoint['host']} closed the mailbox connection", retryable=True, server_text=server_text(str(error))) from None
        except imaplib.IMAP4.error as error:
            raise MailError("protocol", f"{self.endpoint['host']} rejected UID {command}", server_text=server_text(str(error))) from None
        except (OSError, socket.timeout) as error:
            raise connection_error(self.endpoint, error) from None
        if kind != "OK":
            raise MailError("protocol", f"{self.endpoint['host']} answered {kind} to UID {command}", server_text=server_text(data))
        return data

    def response_number(self, code):
        _, values = self.imap.response(code)
        for value in values or []:
            if value is None:
                continue
            match = re.match(rb"\s*(\d+)", value if isinstance(value, bytes) else str(value).encode("ascii", "replace"))
            if match:
                return int(match.group(1))
        return None

    def select(self, mailbox, readonly=True):
        try:
            kind, data = self.imap.select(quote_mailbox(mailbox), readonly=readonly)
        except imaplib.IMAP4.abort as error:
            raise MailError("transport", f"{self.endpoint['host']} closed the mailbox connection", retryable=True, server_text=server_text(str(error))) from None
        except imaplib.IMAP4.error as error:
            raise MailError("not_found", f"The mailbox {mailbox!r} could not be opened", server_text=server_text(str(error))) from None
        except (OSError, socket.timeout) as error:
            raise connection_error(self.endpoint, error) from None
        if kind != "OK":
            raise MailError("not_found", f"The mailbox {mailbox!r} could not be opened", server_text=server_text(data))
        uidvalidity = self.response_number("UIDVALIDITY")
        if not uidvalidity:
            raise MailError("protocol", f"{self.endpoint['host']} reported no UIDVALIDITY for {mailbox!r}")
        uidnext = self.response_number("UIDNEXT")
        exists = int(data[0]) if data and isinstance(data[0], bytes) and data[0].strip().isdigit() else 0
        self.selected = (mailbox, readonly)
        return {"uidvalidity": uidvalidity, "uidnext": uidnext, "exists": exists}

    def last_uid(self):
        data = self.uid("FETCH", "*", "(UID)")
        uids = [response.number(b"UID") for response in fetch_responses(data)]
        return max([uid for uid in uids if uid] or [0])

    def uids_after(self, last_uid):
        return sorted(uid for uid in parse_uids(self.uid("SEARCH", None, "UID", f"{last_uid + 1}:*")) if uid > last_uid)

    def search(self, criteria):
        plain, wide = [], []
        for atom, value in criteria:
            if value is None:
                plain.append(atom)
            elif all(ord(character) < 128 for character in value):
                plain.extend([atom, quote(value)])
            else:
                wide.append((atom, value))
        if not wide:
            return set(parse_uids(self.uid("SEARCH", None, *(plain or ["ALL"]))))
        result = None
        for atom, value in wide:
            self.imap.literal = value.encode("utf-8")
            found = set(parse_uids(self.uid("SEARCH", "CHARSET", "UTF-8", *plain, atom)))
            result = found if result is None else result & found
        return result

    def fetch_summary(self, uid, header_fields, header_limit):
        labels = " X-GM-LABELS" if self.has("X-GM-EXT-1") else ""
        items = f"(UID FLAGS INTERNALDATE RFC822.SIZE BODY.PEEK[HEADER.FIELDS ({' '.join(header_fields).upper()})]<0.{header_limit + 1}>{labels})"
        for response in fetch_responses(self.uid("FETCH", str(uid), items)):
            if response.number(b"UID") == uid:
                headers = response.body()
                if headers is None or len(headers) > header_limit:
                    raise MailError("protocol", f"Message {uid} has unreadable or oversized headers")
                return {
                    "uid": uid,
                    "headers": headers,
                    "flags": response.flags(),
                    "internal_date": response.internal_date(),
                    "size": response.number(b"RFC822.SIZE") or 0,
                    "labels": response.labels(),
                }
        return None

    def fetch_raw(self, uid, max_bytes):
        for response in fetch_responses(self.uid("FETCH", str(uid), f"(UID BODY.PEEK[]<0.{max_bytes + 1}>)")):
            if response.number(b"UID") == uid:
                raw = response.body()
                if raw is None:
                    raise MailError("protocol", f"Message {uid} returned no content")
                if len(raw) > max_bytes:
                    raise MailError("refused", f"Message {uid} is larger than {max_bytes} bytes")
                return raw
        return None

    def read_line(self):
        try:
            line = self.imap.readline()
            if not line:
                raise MailError("transport", f"{self.endpoint['host']} closed the mailbox connection", retryable=True)
            literal = LITERAL_RE.search(line)
            if literal:
                self.imap.read(int(literal.group(1)))
                line = line + self.read_line()
        except (OSError, socket.timeout) as error:
            raise connection_error(self.endpoint, error) from None
        return line

    def idle(self, timeout_seconds):
        imap = self.imap
        tag = imap._new_tag()
        try:
            imap.send(tag + b" IDLE\r\n")
        except OSError as error:
            raise connection_error(self.endpoint, error) from None
        changed = False
        while True:
            line = self.read_line()
            if line.startswith(b"+"):
                break
            if line.startswith(tag + b" "):
                imap.tagged_commands.pop(tag, None)
                raise MailError("protocol", f"{self.endpoint['host']} refused IDLE", server_text=server_text(line))
            changed = changed or bool(CHANGE_RE.match(line))
        deadline = time.monotonic() + timeout_seconds
        while not changed:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                break
            if not imap.octopus_reader.has_data():
                ready, _, _ = select.select([imap.sock], [], [], remaining)
                if not ready:
                    break
            line = self.read_line()
            if line.upper().startswith(b"* BYE"):
                raise MailError("transport", f"{self.endpoint['host']} ended the mailbox connection", retryable=True, server_text=server_text(line))
            changed = bool(CHANGE_RE.match(line))
        try:
            imap.send(b"DONE\r\n")
        except OSError as error:
            raise connection_error(self.endpoint, error) from None
        while True:
            line = self.read_line()
            if line.startswith(tag + b" "):
                imap.tagged_commands.pop(tag, None)
                if not line[len(tag) + 1:].upper().startswith(b"OK"):
                    raise MailError("protocol", f"{self.endpoint['host']} ended IDLE with an error", server_text=server_text(line))
                return changed
            changed = changed or bool(CHANGE_RE.match(line))

    def store_flags(self, uids, add, flags):
        self.uid("STORE", uid_set(uids), "+FLAGS.SILENT" if add else "-FLAGS.SILENT", "(" + " ".join(flags) + ")")

    def move(self, uids, destination):
        if self.has("MOVE"):
            self.uid("MOVE", uid_set(uids), quote_mailbox(destination))
            return {"method": "move", "expunged": True}
        self.uid("COPY", uid_set(uids), quote_mailbox(destination))
        self.store_flags(uids, True, ["\\Deleted"])
        if self.has("UIDPLUS"):
            self.uid("EXPUNGE", uid_set(uids))
            return {"method": "copy", "expunged": True}
        return {"method": "copy", "expunged": False}

    def mailboxes(self):
        rows = []
        for item in self.call("list", '""', '"*"'):
            literal_name = None
            if isinstance(item, tuple):
                text, literal_name = re.sub(rb"\{\d+\}$", b"", item[0].rstrip()), item[1]
            else:
                text = item
            if not isinstance(text, bytes):
                continue
            match = LIST_RE.match(text.strip())
            if not match:
                continue
            name = literal_name if literal_name is not None else match.group("name").strip()
            if literal_name is None and name.startswith(b'"') and name.endswith(b'"'):
                name = re.sub(rb"\\(.)", rb"\1", name[1:-1])
            flags = [flag.decode("ascii", "replace") for flag in match.group("flags").split()]
            rows.append({"name": decode_mailbox(name.decode("utf-8", "replace")), "flags": flags})
        return rows

    def special_mailbox(self, attribute):
        for row in self.mailboxes():
            if any(flag.lower() == attribute.lower() for flag in row["flags"]):
                return row["name"]
        return None

    def find_message_id(self, mailbox, message_id):
        self.select(mailbox, readonly=True)
        uids = self.search([("HEADER Message-ID", message_id)])
        return max(uids) if uids else None

    def append(self, mailbox, raw, flags):
        self.call("append", quote_mailbox(mailbox), flags, imaplib.Time2Internaldate(time.time()), raw)

    def close(self):
        try:
            self.imap.logout()
        except Exception:
            try:
                self.imap.shutdown()
            except Exception:
                pass


def dot_stuff(data):
    data = re.sub(rb"(?<!\r)\n", b"\r\n", data)
    data = re.sub(rb"(?m)^\.", b"..", data)
    return data if data.endswith(b"\r\n") else data + b"\r\n"


class SmtpSession:
    def __init__(self, smtp, endpoint):
        self.smtp = smtp
        self.endpoint = endpoint

    @classmethod
    def open(cls, endpoint, address, password, connect_timeout, command_timeout):
        host, port = endpoint["host"], endpoint["port"]
        smtp = None
        try:
            if endpoint["tls"] == "implicit":
                smtp = smtplib.SMTP_SSL(host, port, local_hostname="localhost", timeout=connect_timeout, context=tls_context())
            else:
                smtp = smtplib.SMTP(host, port, local_hostname="localhost", timeout=connect_timeout)
                smtp.ehlo()
                if not smtp.has_extn("starttls"):
                    raise MailError("tls", f"{host} does not offer STARTTLS on port {port}; Octopus never signs in without encryption")
                smtp.starttls(context=tls_context())
            smtp.ehlo()
            smtp.sock.settimeout(command_timeout)
        except (MailError, OSError) as error:
            if smtp is not None:
                smtp.close()
            if isinstance(error, smtplib.SMTPResponseException):
                raise MailError("transport", f"{host} refused the SMTP session", retryable=400 <= error.smtp_code < 500, server_text=server_text(error.smtp_error)) from None
            raise connection_error(endpoint, error) from None
        session = cls(smtp, endpoint)
        try:
            session.sign_in(address, password)
        except BaseException:
            session.close()
            raise
        return session

    def command(self, *args):
        try:
            code, response = self.smtp.docmd(*args)
        except (OSError, smtplib.SMTPException) as error:
            raise MailError("transport", f"The SMTP connection to {self.endpoint['host']} was lost", retryable=True, server_text=server_text(str(error))) from None
        return code, response

    def sign_in(self, address, password):
        mechanisms = (self.smtp.esmtp_features.get("auth") or "").upper().split()
        encode = lambda value: base64.b64encode(value).decode("ascii")
        if "PLAIN" in mechanisms:
            token = encode(b"\0" + address.encode("utf-8") + b"\0" + password.encode("utf-8"))
            code, response = self.command("AUTH", "PLAIN " + token)
            if code == 334:
                code, response = self.command(token)
        elif "LOGIN" in mechanisms:
            code, response = self.command("AUTH", "LOGIN")
            if code == 334:
                code, response = self.command(encode(address.encode("utf-8")))
            if code == 334:
                code, response = self.command(encode(password.encode("utf-8")))
        else:
            raise MailError("authentication", f"{self.endpoint['host']} offers no password sign-in for SMTP")
        if code != 235:
            temporary = 400 <= code < 500
            raise MailError("transport" if temporary else "authentication", f"{self.endpoint['host']} rejected the SMTP sign-in", retryable=temporary, server_text=f"{code} {server_text(response)}")

    def submit(self, sender, recipients, data, before_commit):
        code, response = self.command("MAIL", f"FROM:<{sender}>")
        if code != 250:
            raise MailError("refused", "The SMTP server refused the sender address", retryable=400 <= code < 500, server_text=f"{code} {server_text(response)}")
        accepted, refused = [], {}
        for recipient in recipients:
            code, response = self.command("RCPT", f"TO:<{recipient}>")
            if code in (250, 251):
                accepted.append(recipient)
            else:
                refused[recipient] = f"{code} {server_text(response, 160)}"
        if not accepted:
            self.command("RSET")
            raise MailError("refused", "The SMTP server refused every recipient", retryable=all(value.startswith("4") for value in refused.values()), server_text="; ".join(f"{key}: {value}" for key, value in refused.items())[:300])
        code, response = self.command("DATA")
        if code != 354:
            raise MailError("refused", "The SMTP server refused to take the message", retryable=400 <= code < 500, server_text=f"{code} {server_text(response)}")
        try:
            self.smtp.send(dot_stuff(data))
        except (OSError, smtplib.SMTPException) as error:
            raise MailError("transport", f"The SMTP connection to {self.endpoint['host']} was lost", retryable=True, server_text=server_text(str(error))) from None
        before_commit()
        try:
            self.smtp.send(b".\r\n")
            code, response = self.smtp.getreply()
        except (OSError, smtplib.SMTPException) as error:
            raise CommitUncertain(f"The SMTP server did not confirm the message: {server_text(str(error))}") from None
        if code != 250:
            raise MailError("refused", "The SMTP server rejected the message", retryable=400 <= code < 500, server_text=f"{code} {server_text(response)}")
        return {"accepted": accepted, "refused": refused, "reply": f"{code} {server_text(response)}"}

    def close(self):
        try:
            self.smtp.quit()
        except Exception:
            try:
                self.smtp.close()
            except Exception:
                pass
