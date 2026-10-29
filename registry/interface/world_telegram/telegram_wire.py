import base64
import ipaddress
import json
import math
import os
import re
import secrets
import socket
import ssl
import struct
import threading
import time
import urllib.parse

from telegram_contract import parse_fallback_ip


OFFICIAL_HOST = "api.telegram.org"
NOT_SENT = {"transport_error", "rate_limited"}


class TelegramWireError(RuntimeError):
    def __init__(self, category, message, method=None, status=None, error_code=None, description=None, retry_after=None, migrate_to_chat_id=None):
        super().__init__(message)
        self.category = category
        self.method = method
        self.status = status
        self.error_code = error_code
        self.description = description
        self.retry_after = retry_after
        self.migrate_to_chat_id = migrate_to_chat_id

    def details(self):
        details = {"category": self.category, "message": str(self)}
        for key in ("method", "status", "error_code", "description", "retry_after", "migrate_to_chat_id"):
            value = getattr(self, key)
            if value is not None:
                details[key] = value
        return details


def is_loopback_host(value):
    if not isinstance(value, str) or not value:
        return False
    host = value.rstrip(".").lower()
    if host == "localhost" or host.endswith(".localhost"):
        return True
    try:
        return ipaddress.ip_address(host).is_loopback
    except ValueError:
        return False


def parse_base(value):
    if not isinstance(value, str) or value.strip() != value or not value:
        raise ValueError("Telegram Bot API base must be a trimmed absolute URL")
    parsed = urllib.parse.urlsplit(value)
    try:
        port = parsed.port
    except ValueError as error:
        raise ValueError("Telegram Bot API base port is invalid") from error
    if parsed.scheme not in {"https", "http"} or not parsed.hostname or parsed.username or parsed.password or parsed.query or parsed.fragment:
        raise ValueError("Telegram Bot API base must be an HTTPS origin without credentials, query or fragment")
    if parsed.scheme == "http" and not is_loopback_host(parsed.hostname):
        raise ValueError("Telegram Bot API base must use HTTPS outside loopback")
    return {
        "scheme": parsed.scheme,
        "host": parsed.hostname,
        "port": port or (443 if parsed.scheme == "https" else 80),
        "prefix": parsed.path.rstrip("/"),
    }


def parse_proxy(value):
    if not value:
        return None
    if value.strip() != value or any(ord(character) < 33 or ord(character) == 127 for character in value) or "\\" in value:
        raise ValueError("Telegram proxy URL contains invalid characters")
    parsed = urllib.parse.urlsplit(value)
    try:
        port = parsed.port
    except ValueError as error:
        raise ValueError("Telegram proxy URL port is invalid") from error
    if parsed.scheme not in {"http", "https", "socks5", "socks5h"} or not parsed.hostname or parsed.path not in {"", "/"} or parsed.query or parsed.fragment:
        raise ValueError("Telegram proxy must be an http, https, socks5 or socks5h URL without path, query or fragment")
    username = urllib.parse.unquote(parsed.username) if parsed.username is not None else None
    password = urllib.parse.unquote(parsed.password) if parsed.password is not None else None
    if (username is None) != (password is None) or username == "" or password == "":
        raise ValueError("Telegram proxy credentials require a non-empty username and password")
    if parsed.scheme == "http" and not is_loopback_host(parsed.hostname):
        raise ValueError("remote Telegram HTTP proxies must use HTTPS")
    if parsed.scheme.startswith("socks5") and not is_loopback_host(parsed.hostname):
        raise ValueError("Telegram SOCKS5 proxies must listen on loopback; use an HTTPS proxy for a remote host")
    default_port = {"http": 80, "https": 443}.get(parsed.scheme, 1080)
    return {"scheme": parsed.scheme, "host": parsed.hostname, "port": port or default_port, "username": username, "password": password, "url": value}


class TlsTunnel:
    def __init__(self, transport, server_hostname, timeout):
        self.transport = transport
        self.incoming = ssl.MemoryBIO()
        self.outgoing = ssl.MemoryBIO()
        self.tls = ssl.create_default_context().wrap_bio(self.incoming, self.outgoing, server_side=False, server_hostname=server_hostname)
        self.transport.settimeout(timeout)
        self._drive(self.tls.do_handshake)

    def _drive(self, operation):
        while True:
            try:
                result = operation()
                while self.outgoing.pending:
                    self.transport.sendall(self.outgoing.read())
                return result
            except ssl.SSLWantReadError:
                while self.outgoing.pending:
                    self.transport.sendall(self.outgoing.read())
                payload = self.transport.recv(65536)
                if payload:
                    self.incoming.write(payload)
                else:
                    self.incoming.write_eof()

    def sendall(self, payload):
        view = memoryview(payload)
        while view:
            written = self._drive(lambda: self.tls.write(view[:65536]))
            view = view[written:]

    def recv(self, maximum):
        try:
            return self._drive(lambda: self.tls.read(maximum))
        except (ssl.SSLZeroReturnError, ssl.SSLEOFError):
            return b""

    def settimeout(self, timeout):
        self.transport.settimeout(timeout)

    def close(self):
        self.transport.close()


def read_exact(sock, size):
    data = bytearray()
    while len(data) < size:
        chunk = sock.recv(size - len(data))
        if not chunk:
            raise OSError("Telegram proxy closed the connection during its handshake")
        data.extend(chunk)
    return bytes(data)


def socks5_connect(proxy, target, port, timeout):
    sock = socket.create_connection((proxy["host"], proxy["port"]), timeout=timeout)
    try:
        methods = b"\x00\x02" if proxy["username"] is not None else b"\x00"
        sock.sendall(b"\x05" + bytes([len(methods)]) + methods)
        version, method = read_exact(sock, 2)
        if version != 5 or method not in (0, 2) or (method == 2 and proxy["username"] is None):
            raise OSError("Telegram SOCKS5 proxy refused the offered authentication")
        if method == 2:
            user, secret = proxy["username"].encode("utf-8"), proxy["password"].encode("utf-8")
            if len(user) > 255 or len(secret) > 255:
                raise ValueError("Telegram SOCKS5 credentials exceed 255 bytes")
            sock.sendall(b"\x01" + bytes([len(user)]) + user + bytes([len(secret)]) + secret)
            if read_exact(sock, 2) != b"\x01\x00":
                raise OSError("Telegram SOCKS5 proxy rejected its credentials")
        try:
            address = ipaddress.ip_address(target)
            encoded = (b"\x01" if address.version == 4 else b"\x04") + address.packed
        except ValueError:
            if proxy["scheme"] == "socks5h":
                name = target.encode("idna")
                encoded = b"\x03" + bytes([len(name)]) + name
            else:
                resolved = ipaddress.ip_address(socket.getaddrinfo(target, port, type=socket.SOCK_STREAM)[0][4][0])
                encoded = (b"\x01" if resolved.version == 4 else b"\x04") + resolved.packed
        sock.sendall(b"\x05\x01\x00" + encoded + struct.pack("!H", port))
        version, status, _, kind = read_exact(sock, 4)
        if version != 5 or status != 0:
            raise OSError(f"Telegram SOCKS5 proxy could not connect (status {status})")
        if kind not in (1, 3, 4):
            raise OSError("Telegram SOCKS5 proxy answered with an invalid address type")
        read_exact(sock, (4 if kind == 1 else 16 if kind == 4 else read_exact(sock, 1)[0]) + 2)
        return sock
    except Exception:
        sock.close()
        raise


def http_connect(proxy, target, port, timeout):
    sock = socket.create_connection((proxy["host"], proxy["port"]), timeout=timeout)
    try:
        if proxy["scheme"] == "https":
            sock = ssl.create_default_context().wrap_socket(sock, server_hostname=proxy["host"])
        authority = f"[{target}]:{port}" if ":" in target else f"{target}:{port}"
        lines = [f"CONNECT {authority} HTTP/1.1", f"Host: {authority}", "Proxy-Connection: keep-alive"]
        if proxy["username"] is not None:
            encoded = base64.b64encode(f"{proxy['username']}:{proxy['password']}".encode("utf-8")).decode("ascii")
            lines.append(f"Proxy-Authorization: Basic {encoded}")
        sock.sendall(("\r\n".join(lines) + "\r\n\r\n").encode("ascii"))
        response = bytearray()
        while b"\r\n\r\n" not in response:
            chunk = sock.recv(4096)
            if not chunk:
                raise OSError("Telegram proxy closed during CONNECT")
            response.extend(chunk)
            if len(response) > 65536:
                raise OSError("Telegram proxy CONNECT response is too large")
        head, extra = bytes(response).split(b"\r\n\r\n", 1)
        status = re.match(rb"HTTP/1\.[01] ([0-9]{3})", head)
        if status is None or status.group(1) != b"200" or extra:
            raise OSError(f"Telegram proxy refused CONNECT{' with status ' + status.group(1).decode() if status else ''}")
        return sock
    except Exception:
        sock.close()
        raise


def read_response(sock, maximum, method):
    buffer = bytearray()
    while b"\r\n\r\n" not in buffer:
        chunk = sock.recv(65536)
        if not chunk:
            raise TelegramWireError("transport_uncertain", f"Telegram closed the connection before answering {method}", method=method)
        buffer.extend(chunk)
        if len(buffer) > 65536 and b"\r\n\r\n" not in buffer:
            raise TelegramWireError("malformed_response", f"Telegram response headers for {method} are too large", method=method)
    head, body = bytes(buffer).split(b"\r\n\r\n", 1)
    lines = head.split(b"\r\n")
    status = re.fullmatch(rb"HTTP/1\.[01] ([0-9]{3})(?: .*)?", lines[0])
    if status is None:
        raise TelegramWireError("malformed_response", f"Telegram answered {method} with an invalid status line", method=method)
    headers = {}
    for line in lines[1:]:
        name, separator, value = line.partition(b":")
        if not separator:
            raise TelegramWireError("malformed_response", f"Telegram answered {method} with a malformed header", method=method)
        key = name.decode("latin-1").strip().lower()
        if key in headers and key in {"content-length", "transfer-encoding"}:
            raise TelegramWireError("malformed_response", f"Telegram answered {method} with repeated framing headers", method=method)
        headers[key] = value.decode("latin-1").strip()
    body = bytearray(body)

    def fill(size):
        while len(body) < size:
            chunk = sock.recv(min(65536, size - len(body)))
            if not chunk:
                raise TelegramWireError("transport_uncertain", f"Telegram response to {method} ended early", method=method)
            body.extend(chunk)

    if headers.get("transfer-encoding", "").lower() == "chunked":
        decoded = bytearray()
        while True:
            while b"\r\n" not in body:
                fill(len(body) + 1)
                if len(body) > 4096 and b"\r\n" not in body:
                    raise TelegramWireError("malformed_response", f"Telegram response to {method} has a malformed chunk", method=method)
            line, _, rest = bytes(body).partition(b"\r\n")
            body = bytearray(rest)
            try:
                size = int(line.split(b";", 1)[0].strip(), 16)
            except ValueError as error:
                raise TelegramWireError("malformed_response", f"Telegram response to {method} has a malformed chunk size", method=method) from error
            if size == 0:
                break
            if len(decoded) + size > maximum:
                raise TelegramWireError("response_too_large", f"Telegram response to {method} exceeds {maximum} bytes", method=method)
            fill(size + 2)
            decoded.extend(body[:size])
            body = body[size + 2 :]
        return int(status.group(1)), headers, bytes(decoded)
    if "content-length" in headers:
        try:
            length = int(headers["content-length"])
        except ValueError as error:
            raise TelegramWireError("malformed_response", f"Telegram response to {method} has an invalid Content-Length", method=method) from error
        if length < 0 or length > maximum:
            raise TelegramWireError("response_too_large", f"Telegram response to {method} declares {length} bytes", method=method)
        fill(length)
        return int(status.group(1)), headers, bytes(body[:length])
    while True:
        chunk = sock.recv(65536)
        if not chunk:
            break
        body.extend(chunk)
        if len(body) > maximum:
            raise TelegramWireError("response_too_large", f"Telegram response to {method} exceeds {maximum} bytes", method=method)
    return int(status.group(1)), headers, bytes(body)


class BotApi:
    def __init__(self, config, token, sanitize=None):
        transport = config["transport"]
        self.config = config
        self.token = token
        self.sanitize = sanitize or (lambda value: value)
        self.base = parse_base(os.environ.get(transport["api_base_env"], "").strip() or transport["api_base_default"])
        self.proxy = parse_proxy(os.environ.get(transport["proxy_env"], "").strip())
        override = os.environ.get(transport["fallback_ips_env"], "").strip()
        addresses = [part for part in re.split(r"[\s,]+", override) if part] if override else transport["fallback_ips"]
        fallback = [parse_fallback_ip(address, transport["fallback_ips_env"] if override else "Telegram transport.fallback_ips") for address in addresses]
        self.fallback_ips = list(dict.fromkeys(fallback)) if self.base["host"] == OFFICIAL_HOST and self.base["scheme"] == "https" else []
        self.connect_timeout = transport["connect_timeout_ms"] / 1000.0
        self.request_timeout = transport["request_timeout_ms"] / 1000.0
        self.maximum = transport["max_response_bytes"]
        self.rate_limit_max_wait = transport["rate_limit_max_wait_ms"] / 1000.0
        self.rate_limit_retries = transport["rate_limit_retries"]
        self.read_retry_limit = transport["read_retry_limit"]
        self.sticky = None
        self.lock = threading.Lock()

    def _targets(self):
        with self.lock:
            sticky = self.sticky
        if self.proxy is not None:
            names = [self.base["host"]]
        else:
            try:
                resolved = socket.getaddrinfo(self.base["host"], self.base["port"], type=socket.SOCK_STREAM)
            except OSError:
                resolved = []
            names = list(dict.fromkeys(entry[4][0] for entry in sorted(resolved, key=lambda entry: entry[0] != socket.AF_INET)))
        targets = list(dict.fromkeys(([sticky] if sticky else []) + names + self.fallback_ips))
        return targets or [self.base["host"]]

    def _open(self, target, timeout):
        host, port = self.base["host"], self.base["port"]
        if self.proxy is None:
            sock = socket.create_connection((target, port), timeout=timeout)
        elif self.proxy["scheme"].startswith("socks5"):
            sock = socks5_connect(self.proxy, target, port, timeout)
        else:
            sock = http_connect(self.proxy, target, port, timeout)
        try:
            if self.base["scheme"] != "https":
                return sock
            if isinstance(sock, ssl.SSLSocket):
                return TlsTunnel(sock, host, timeout)
            return ssl.create_default_context().wrap_socket(sock, server_hostname=host)
        except Exception:
            sock.close()
            raise

    def _exchange(self, verb, path, body, content_type, method, timeout, maximum):
        host_header = self.base["host"] if self.base["port"] in (80, 443) else f"{self.base['host']}:{self.base['port']}"
        request = [f"{verb} {self.base['prefix']}{path} HTTP/1.1", f"Host: {host_header}", "User-Agent: octopus-world-telegram/1", "Accept: application/json", "Connection: close"]
        if body is not None:
            request += [f"Content-Type: {content_type}", f"Content-Length: {len(body)}"]
        payload = ("\r\n".join(request) + "\r\n\r\n").encode("utf-8") + (body or b"")
        failures = []
        for target in self._targets():
            try:
                sock = self._open(target, self.connect_timeout)
            except (OSError, ssl.SSLError, ValueError) as error:
                failures.append(f"{target}: {type(error).__name__}")
                with self.lock:
                    if self.sticky == target:
                        self.sticky = None
                continue
            try:
                sock.settimeout(timeout)
                sock.sendall(payload)
                response = read_response(sock, maximum, method)
            except TelegramWireError:
                raise
            except (OSError, ssl.SSLError) as error:
                raise TelegramWireError("transport_uncertain", f"Telegram connection failed during {method} after the request was sent ({type(error).__name__})", method=method) from None
            finally:
                try:
                    sock.close()
                except OSError:
                    pass
            with self.lock:
                self.sticky = target
            return response
        raise TelegramWireError("transport_error", f"Telegram is unreachable for {method}: " + "; ".join(failures[-4:]), method=method)

    def _decoded(self, method, status, raw):
        try:
            response = json.loads(raw)
        except (UnicodeDecodeError, json.JSONDecodeError):
            raise TelegramWireError("malformed_response" if status < 500 else "http_error", f"Telegram answered {method} with HTTP {status} and no JSON body", method=method, status=status) from None
        if not isinstance(response, dict):
            raise TelegramWireError("malformed_response", f"Telegram answered {method} with a non-object", method=method, status=status)
        if response.get("ok") is True and status == 200:
            return response.get("result")
        code = response.get("error_code") if isinstance(response.get("error_code"), int) else status
        description = self.sanitize(response.get("description")) if isinstance(response.get("description"), str) else None
        parameters = response.get("parameters") if isinstance(response.get("parameters"), dict) else {}
        retry_after = parameters.get("retry_after") if isinstance(parameters.get("retry_after"), (int, float)) and not isinstance(parameters.get("retry_after"), bool) else None
        migrate = parameters.get("migrate_to_chat_id") if isinstance(parameters.get("migrate_to_chat_id"), int) else None
        category = {401: "unauthorized", 404: "unauthorized", 409: "conflict", 429: "rate_limited"}.get(code, "http_error" if code >= 500 else "telegram_error")
        raise TelegramWireError(category, f"Telegram {method} failed with {code}{': ' + description if description else ''}", method=method, status=status, error_code=code, description=description, retry_after=retry_after, migrate_to_chat_id=migrate)

    def _request(self, verb, path, body, content_type, method, timeout, retry_safe, maximum, decode=True):
        attempt = 0
        rate_waits = 0
        while True:
            try:
                status, headers, raw = self._exchange(verb, path, body, content_type, method, timeout, maximum)
                return self._decoded(method, status, raw) if decode else (status, headers, raw)
            except TelegramWireError as error:
                if error.category == "rate_limited" and rate_waits < self.rate_limit_retries:
                    wait = float(error.retry_after) if error.retry_after is not None else 1.0
                    if math.isfinite(wait) and 0 <= wait <= self.rate_limit_max_wait:
                        rate_waits += 1
                        time.sleep(wait)
                        continue
                if retry_safe and error.category in {"transport_error", "transport_uncertain", "http_error"} and attempt < self.read_retry_limit:
                    time.sleep(min(0.5 * (2 ** attempt), 4.0))
                    attempt += 1
                    continue
                raise

    def call(self, method, params=None, timeout=None, retry_safe=False):
        if not re.fullmatch(r"[a-z][A-Za-z]{1,63}", method):
            raise ValueError("Telegram Bot API method name is malformed")
        body = json.dumps(params or {}, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
        return self._request("POST", f"/bot{self.token}/{method}", body, "application/json", method, timeout or self.request_timeout, retry_safe, self.maximum)

    def call_multipart(self, method, fields, files):
        boundary = "octopus-" + secrets.token_hex(16)
        body = bytearray()
        for name, value in fields.items():
            if value is None:
                continue
            encoded = json.dumps(value, ensure_ascii=False, separators=(",", ":")) if isinstance(value, (dict, list, bool)) else str(value)
            body += f'--{boundary}\r\nContent-Disposition: form-data; name="{name}"\r\n\r\n'.encode("utf-8") + encoded.encode("utf-8") + b"\r\n"
        for name, (filename, content_type, content) in files.items():
            safe_name = re.sub(r'[^A-Za-z0-9._ -]', "_", filename)[:200] or "upload.bin"
            body += f'--{boundary}\r\nContent-Disposition: form-data; name="{name}"; filename="{safe_name}"\r\nContent-Type: {content_type}\r\n\r\n'.encode("utf-8") + content + b"\r\n"
        body += f"--{boundary}--\r\n".encode("ascii")
        return self._request("POST", f"/bot{self.token}/{method}", bytes(body), f"multipart/form-data; boundary={boundary}", method, self.request_timeout, False, self.maximum)

    def download(self, file_path, maximum):
        if not isinstance(file_path, str) or not file_path or file_path.startswith("/") or ".." in file_path.split("/") or re.search(r"[\s?#%\\]", file_path):
            raise TelegramWireError("boundary_rejected", "Telegram returned a file path outside its file store", method="download")
        status, _, raw = self._request("GET", f"/file/bot{self.token}/{urllib.parse.quote(file_path, safe='/')}", None, None, "download", self.request_timeout, True, maximum, decode=False)
        if status != 200:
            raise TelegramWireError("http_error" if status >= 500 else "telegram_error", f"Telegram file download failed with HTTP {status}", method="download", status=status)
        return raw
