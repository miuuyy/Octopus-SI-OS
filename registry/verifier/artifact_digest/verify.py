#!/usr/bin/env python3
"""Deterministic streaming SHA-256, BLAKE3-256, and byte-bound artifact verifier."""

import hashlib
import hmac
import json
import math
import os
import re
import stat
import struct
import sys

HANDLER = "artifact_digest"
CHUNK_READ_SIZE = 65536
HEX64_RE = re.compile(r"^[0-9a-f]{64}$")

ALLOWED_PARAM_KEYS = {
    "sha256",
    "blake3",
    "match_artifact_content_hash",
    "min_bytes",
    "max_bytes",
    "exact_bytes",
    "timeout_ms",
}

# Self-contained pure-Python streaming BLAKE3-256 (standard reference algorithm,
# byte-identical to octopus_ledger::ContentHash::of_bytes).
_MASK = 0xFFFFFFFF
_IV = (
    0x6A09E667,
    0xBB67AE85,
    0x3C6EF372,
    0xA54FF53A,
    0x510E527F,
    0x9B05688C,
    0x1F83D9AB,
    0x5BE0CD19,
)
_PERMUTATION = (2, 6, 3, 10, 7, 0, 4, 13, 1, 11, 12, 5, 9, 14, 15, 8)
_CHUNK_START = 1
_CHUNK_END = 2
_PARENT = 4
_ROOT = 8
_BLOCK_LEN = 64
_CHUNK_LEN = 1024


def _rotate_right(value, count):
    return ((value >> count) | (value << (32 - count))) & _MASK


def _mix(state, a, b, c, d, x, y):
    state[a] = (state[a] + state[b] + x) & _MASK
    state[d] = _rotate_right(state[d] ^ state[a], 16)
    state[c] = (state[c] + state[d]) & _MASK
    state[b] = _rotate_right(state[b] ^ state[c], 12)
    state[a] = (state[a] + state[b] + y) & _MASK
    state[d] = _rotate_right(state[d] ^ state[a], 8)
    state[c] = (state[c] + state[d]) & _MASK
    state[b] = _rotate_right(state[b] ^ state[c], 7)


def _round(state, message):
    _mix(state, 0, 4, 8, 12, message[0], message[1])
    _mix(state, 1, 5, 9, 13, message[2], message[3])
    _mix(state, 2, 6, 10, 14, message[4], message[5])
    _mix(state, 3, 7, 11, 15, message[6], message[7])
    _mix(state, 0, 5, 10, 15, message[8], message[9])
    _mix(state, 1, 6, 11, 12, message[10], message[11])
    _mix(state, 2, 7, 8, 13, message[12], message[13])
    _mix(state, 3, 4, 9, 14, message[14], message[15])


def _compress(chaining_value, block_words, counter, block_len, flags):
    state = list(chaining_value) + list(_IV[:4]) + [
        counter & _MASK,
        (counter >> 32) & _MASK,
        block_len,
        flags,
    ]
    message = list(block_words)
    for round_index in range(7):
        _round(state, message)
        if round_index != 6:
            message = [message[index] for index in _PERMUTATION]
    return tuple(state[index] ^ state[index + 8] for index in range(8)) + tuple(
        state[index + 8] ^ chaining_value[index] for index in range(8)
    )


def _block_words(value):
    if len(value) > _BLOCK_LEN:
        raise ValueError("BLAKE3 block exceeds 64 bytes")
    return struct.unpack("<16I", value.ljust(_BLOCK_LEN, b"\0"))


class _Output:
    def __init__(self, input_chaining_value, block_words, counter, block_len, flags):
        self.input_chaining_value = input_chaining_value
        self.block_words = block_words
        self.counter = counter
        self.block_len = block_len
        self.flags = flags

    def chaining_value(self):
        return _compress(
            self.input_chaining_value,
            self.block_words,
            self.counter,
            self.block_len,
            self.flags,
        )[:8]

    def root_hash(self):
        words = _compress(
            self.input_chaining_value,
            self.block_words,
            0,
            self.block_len,
            self.flags | _ROOT,
        )
        return struct.pack("<16I", *words)[:32]


class _ChunkState:
    def __init__(self, chunk_counter):
        self.chaining_value = _IV
        self.chunk_counter = chunk_counter
        self.block = bytearray()
        self.blocks_compressed = 0

    def length(self):
        return self.blocks_compressed * _BLOCK_LEN + len(self.block)

    def start_flag(self):
        return _CHUNK_START if self.blocks_compressed == 0 else 0

    def update(self, value):
        view = memoryview(value)
        offset = 0
        while offset < len(view):
            if len(self.block) == _BLOCK_LEN:
                self.chaining_value = _compress(
                    self.chaining_value,
                    _block_words(bytes(self.block)),
                    self.chunk_counter,
                    _BLOCK_LEN,
                    self.start_flag(),
                )[:8]
                self.blocks_compressed += 1
                self.block.clear()
            take = min(_BLOCK_LEN - len(self.block), len(view) - offset)
            self.block.extend(view[offset : offset + take])
            offset += take

    def output(self):
        return _Output(
            self.chaining_value,
            _block_words(bytes(self.block)),
            self.chunk_counter,
            len(self.block),
            self.start_flag() | _CHUNK_END,
        )


def _parent_output(left, right):
    return _Output(_IV, tuple(left) + tuple(right), 0, _BLOCK_LEN, _PARENT)


class _Blake3Hasher:
    def __init__(self):
        self.chunk_state = _ChunkState(0)
        self.chaining_values = []

    def _add_chunk(self, chaining_value, total_chunks):
        while total_chunks & 1 == 0:
            chaining_value = _parent_output(
                self.chaining_values.pop(), chaining_value
            ).chaining_value()
            total_chunks >>= 1
        self.chaining_values.append(chaining_value)

    def update(self, value):
        view = memoryview(value)
        offset = 0
        while offset < len(view):
            if self.chunk_state.length() == _CHUNK_LEN:
                chunk_value = self.chunk_state.output().chaining_value()
                total_chunks = self.chunk_state.chunk_counter + 1
                self._add_chunk(chunk_value, total_chunks)
                self.chunk_state = _ChunkState(total_chunks)
            take = min(_CHUNK_LEN - self.chunk_state.length(), len(view) - offset)
            self.chunk_state.update(view[offset : offset + take])
            offset += take
        return self

    def hexdigest(self):
        output = self.chunk_state.output()
        for left in reversed(self.chaining_values):
            output = _parent_output(left, output.chaining_value())
        return output.root_hash().hex()


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


def is_hex64(value):
    return isinstance(value, str) and HEX64_RE.match(value) is not None


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
    if not keys.issubset(ALLOWED_PARAM_KEYS):
        fail("params_shape_invalid", keys=sorted(keys))
        return

    if "timeout_ms" in params:
        timeout_ms = params["timeout_ms"]
        if not is_strict_int(timeout_ms) or timeout_ms < 1:
            fail("params_invalid", field="timeout_ms")
            return

    expected_sha256 = None
    if "sha256" in params:
        expected_sha256 = params["sha256"]
        if not is_hex64(expected_sha256):
            fail("params_invalid", field="sha256")
            return

    expected_blake3 = None
    if "blake3" in params:
        expected_blake3 = params["blake3"]
        if not is_hex64(expected_blake3):
            fail("params_invalid", field="blake3")
            return

    match_artifact_content_hash = params.get("match_artifact_content_hash", False)
    if type(match_artifact_content_hash) is not bool:
        fail("params_invalid", field="match_artifact_content_hash")
        return

    min_bytes = None
    if "min_bytes" in params:
        min_bytes = params["min_bytes"]
        if not is_strict_int(min_bytes) or min_bytes < 0:
            fail("params_invalid", field="min_bytes")
            return

    max_bytes = None
    if "max_bytes" in params:
        max_bytes = params["max_bytes"]
        if not is_strict_int(max_bytes) or max_bytes < 0:
            fail("params_invalid", field="max_bytes")
            return

    exact_bytes = None
    if "exact_bytes" in params:
        exact_bytes = params["exact_bytes"]
        if not is_strict_int(exact_bytes) or exact_bytes < 0:
            fail("params_invalid", field="exact_bytes")
            return

    if min_bytes is not None and max_bytes is not None and min_bytes > max_bytes:
        fail("params_invalid", field="min_bytes", message="min_bytes exceeds max_bytes")
        return
    if exact_bytes is not None:
        if min_bytes is not None and exact_bytes < min_bytes:
            fail("params_invalid", field="exact_bytes", message="exact_bytes below min_bytes")
            return
        if max_bytes is not None and exact_bytes > max_bytes:
            fail("params_invalid", field="exact_bytes", message="exact_bytes above max_bytes")
            return

    checks = []
    if expected_sha256 is not None:
        checks.append("sha256")
    if expected_blake3 is not None:
        checks.append("blake3")
    if match_artifact_content_hash:
        checks.append("match_artifact_content_hash")
    if min_bytes is not None:
        checks.append("min_bytes")
    if max_bytes is not None:
        checks.append("max_bytes")
    if exact_bytes is not None:
        checks.append("exact_bytes")

    if not checks:
        fail("no_constraints_declared")
        return

    expected_artifact_content = None
    if match_artifact_content_hash:
        artifact = request.get("artifact")
        content = artifact.get("content") if isinstance(artifact, dict) else None
        if not is_hex64(content):
            fail("artifact_content_hash_invalid")
            return
        expected_artifact_content = content

    payload_path = request.get("payload_path")
    if not isinstance(payload_path, str) or not payload_path:
        fail("payload_unavailable", message="payload_path is missing")
        return

    try:
        lstat_info = os.lstat(payload_path)
    except OSError as err:
        fail("payload_unavailable", message=str(err))
        return

    if stat.S_ISLNK(lstat_info.st_mode) or not stat.S_ISREG(lstat_info.st_mode):
        fail("payload_unavailable", message="payload_path is not a regular file")
        return

    flags = os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0)
    try:
        fd = os.open(payload_path, flags)
    except OSError as err:
        fail("payload_unavailable", message=str(err))
        return

    sha256_hasher = hashlib.sha256() if expected_sha256 is not None else None
    blake3_hasher = (
        _Blake3Hasher()
        if (expected_blake3 is not None or match_artifact_content_hash)
        else None
    )
    byte_length = 0

    try:
        fstat_info = os.fstat(fd)
        if not stat.S_ISREG(fstat_info.st_mode):
            fail("payload_unavailable", message="payload descriptor is not a regular file")
            return
        with os.fdopen(fd, "rb", closefd=False) as handle:
            while True:
                chunk = handle.read(CHUNK_READ_SIZE)
                if not chunk:
                    break
                byte_length += len(chunk)
                if sha256_hasher is not None:
                    sha256_hasher.update(chunk)
                if blake3_hasher is not None:
                    blake3_hasher.update(chunk)
    except OSError as err:
        fail("payload_unavailable", message=str(err))
        return
    finally:
        os.close(fd)

    if min_bytes is not None and byte_length < min_bytes:
        fail("size_below_min", byte_length=byte_length, min_bytes=min_bytes)
        return
    if max_bytes is not None and byte_length > max_bytes:
        fail("size_above_max", byte_length=byte_length, max_bytes=max_bytes)
        return
    if exact_bytes is not None and byte_length != exact_bytes:
        fail("size_mismatch", byte_length=byte_length, exact_bytes=exact_bytes)
        return

    actual_sha256 = sha256_hasher.hexdigest() if sha256_hasher is not None else None
    actual_blake3 = blake3_hasher.hexdigest() if blake3_hasher is not None else None

    if expected_sha256 is not None and not hmac.compare_digest(actual_sha256, expected_sha256):
        fail("sha256_mismatch", byte_length=byte_length, expected=expected_sha256, actual=actual_sha256)
        return

    if expected_blake3 is not None and not hmac.compare_digest(actual_blake3, expected_blake3):
        fail("blake3_mismatch", byte_length=byte_length, expected=expected_blake3, actual=actual_blake3)
        return

    if match_artifact_content_hash and not hmac.compare_digest(actual_blake3, expected_artifact_content):
        fail(
            "artifact_content_hash_mismatch",
            byte_length=byte_length,
            expected=expected_artifact_content,
            actual=actual_blake3,
        )
        return

    details = {
        "handler": HANDLER,
        "byte_length": byte_length,
        "checks": checks,
    }
    if actual_sha256 is not None:
        details["sha256"] = actual_sha256
    if actual_blake3 is not None:
        details["blake3"] = actual_blake3
    verdict(True, details)


if __name__ == "__main__":
    main()
