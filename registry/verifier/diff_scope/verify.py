#!/usr/bin/env python3
"""Deterministic diff path-scope, anti-spoofing, and hygiene verifier."""

import json
import math
import os
import re
import stat
import sys

HANDLER = "diff_scope"
DEFAULT_MAX_PAYLOAD_BYTES = 4 * 1024 * 1024
MAX_ALLOWED_PAYLOAD_BYTES = 16 * 1024 * 1024

ALLOWED_PARAM_KEYS = {
    "allowed_prefixes",
    "denied_prefixes",
    "format",
    "allow_empty",
    "allow_binary",
    "max_files",
    "max_changed_lines",
    "reject_secret_patterns",
    "max_payload_bytes",
    "timeout_ms",
}
VALID_FORMATS = {"auto", "unified", "begin_patch"}

HUNK_RE = re.compile(r"^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@")
MODE_RE = re.compile(r"^(?:old mode|new mode|new file mode|deleted file mode)\s+([0-7]+)$")
BINARY_FILES_RE = re.compile(r"^Binary files (.+) and (.+) differ$")

SECRET_PATTERNS = (
    ("private_key_pem", re.compile(r"-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----")),
    ("github_pat", re.compile(r"\bgh[pousr]_[A-Za-z0-9]{20,}\b")),
    ("github_fine_grained_pat", re.compile(r"\bgithub_pat_[A-Za-z0-9_]{20,}\b")),
    ("aws_access_key_id", re.compile(r"\bAKIA[0-9A-Z]{16}\b")),
    ("api_secret_key", re.compile(r"\bsk-(?:live|test|proj)-[A-Za-z0-9_-]{16,}\b")),
)


class DiffError(Exception):
    def __init__(self, reason, **details):
        super().__init__(reason)
        self.reason = reason
        self.details = details


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


def has_control_or_nul(text):
    for ch in text:
        code = ord(ch)
        if code < 0x20 or code == 0x7F:
            return True
    return False


def is_windows_drive_path(text):
    return len(text) >= 2 and text[1] == ":" and text[0].isalpha()


def normalize_rel_path(raw):
    if not isinstance(raw, str) or not raw:
        raise DiffError("unsafe_path", message="empty path")
    if raw.startswith('"') or raw.endswith('"'):
        raise DiffError("unsafe_path", message="quoted git path is not supported")
    if has_control_or_nul(raw):
        raise DiffError("unsafe_path", message="path contains control characters")
    if raw.startswith("/") or "\\" in raw or is_windows_drive_path(raw):
        raise DiffError("unsafe_path", message="absolute or backslash path is forbidden")
    if raw.endswith("/") or "//" in raw:
        raise DiffError("unsafe_path", message="path contains empty segment")
    parts = raw.split("/")
    for part in parts:
        if not part or part in (".", ".."):
            raise DiffError("unsafe_path", message="path traversal segment is forbidden")
    return raw


def validate_prefix(raw):
    if not isinstance(raw, str) or not raw:
        return False
    if has_control_or_nul(raw):
        return False
    if raw.startswith("/") or "\\" in raw or is_windows_drive_path(raw):
        return False
    if "//" in raw:
        return False
    trimmed = raw[:-1] if raw.endswith("/") else raw
    if not trimmed:
        return False
    parts = trimmed.split("/")
    for part in parts:
        if not part or part in (".", ".."):
            return False
    return True


def matches_prefix(rel_path, prefix):
    if prefix.endswith("/"):
        return rel_path.startswith(prefix)
    return rel_path == prefix


def check_secret_line(added_text, current_path, line_index, reject_secret_patterns):
    if not reject_secret_patterns:
        return
    for rule_name, pattern in SECRET_PATTERNS:
        if pattern.search(added_text) is not None:
            raise DiffError(
                "secret_pattern_detected",
                path=current_path or "",
                rule=rule_name,
                line_index=line_index,
            )


def split_git_diff_header(line):
    prefix = "diff --git "
    rest = line[len(prefix) :]
    if '"' in rest:
        raise DiffError("unsafe_path", message="quoted git diff header is not supported")
    if not rest.startswith("a/"):
        raise DiffError("diff_parse_failed", message="malformed diff --git header")
    body = rest[2:]
    marker = " b/"
    positions = []
    start = 0
    while True:
        idx = body.find(marker, start)
        if idx < 0:
            break
        positions.append(idx)
        start = idx + 1
    if not positions:
        raise DiffError("diff_parse_failed", message="malformed diff --git header")
    if len(positions) == 1:
        idx = positions[0]
        p1, p2 = body[:idx], body[idx + len(marker) :]
    else:
        symmetric = [
            (body[:idx], body[idx + len(marker) :])
            for idx in positions
            if body[:idx] == body[idx + len(marker) :]
        ]
        if len(symmetric) == 1:
            p1, p2 = symmetric[0]
        else:
            raise DiffError("diff_parse_failed", message="ambiguous diff --git header")
    return normalize_rel_path(p1), normalize_rel_path(p2)


def strip_header_timestamp(raw):
    if "\t" in raw:
        return raw.split("\t", 1)[0]
    return raw


def parse_binary_side(raw, side_prefix):
    if raw == "/dev/null":
        return None
    if raw.startswith('"') or raw.endswith('"'):
        raise DiffError("unsafe_path", message="quoted binary diff path is not supported")
    if raw.startswith(side_prefix):
        return normalize_rel_path(raw[len(side_prefix) :])
    return normalize_rel_path(raw)


def parse_unified(lines, allow_binary, reject_secret_patterns):
    touched = []
    touched_set = set()
    added_lines = 0
    removed_lines = 0

    def record_path(path):
        if path not in touched_set:
            touched_set.add(path)
            touched.append(path)

    in_git_block = False
    block_p1 = None
    block_p2 = None
    current_path = None
    in_hunk = False
    old_rem = 0
    new_rem = 0
    saw_header = False

    idx = 0
    total = len(lines)
    while idx < total:
        line = lines[idx]
        line_no = idx + 1

        if in_hunk:
            if old_rem == 0 and new_rem == 0:
                if line.startswith("\\"):
                    idx += 1
                    continue
                in_hunk = False
            else:
                if line.startswith("+"):
                    if new_rem <= 0:
                        raise DiffError("diff_parse_failed", line_index=line_no, message="hunk overflow on added line")
                    new_rem -= 1
                    added_lines += 1
                    check_secret_line(line[1:], current_path, line_no, reject_secret_patterns)
                    idx += 1
                    continue
                if line.startswith("-"):
                    if old_rem <= 0:
                        raise DiffError("diff_parse_failed", line_index=line_no, message="hunk overflow on removed line")
                    old_rem -= 1
                    removed_lines += 1
                    idx += 1
                    continue
                if line.startswith(" ") or line == "":
                    if old_rem <= 0 or new_rem <= 0:
                        raise DiffError("diff_parse_failed", line_index=line_no, message="hunk overflow on context line")
                    old_rem -= 1
                    new_rem -= 1
                    idx += 1
                    continue
                if line.startswith("\\"):
                    idx += 1
                    continue
                raise DiffError("diff_parse_failed", line_index=line_no, message="unexpected line inside hunk")

        if line.startswith("diff --git "):
            p1, p2 = split_git_diff_header(line)
            in_git_block = True
            block_p1, block_p2 = p1, p2
            current_path = p2
            saw_header = True
            record_path(p1)
            record_path(p2)
            idx += 1
            continue

        mode_match = MODE_RE.match(line)
        if mode_match:
            mode_val = mode_match.group(1)
            if mode_val.endswith("120000"):
                raise DiffError(
                    "symlink_change_not_allowed",
                    path=current_path or "",
                    mode=mode_val,
                    line_index=line_no,
                )
            idx += 1
            continue

        if line.startswith("rename from ") or line.startswith("copy from "):
            if not in_git_block:
                raise DiffError("diff_parse_failed", line_index=line_no, message="rename/copy outside diff --git block")
            raw_from = line.split(" ", 2)[2]
            norm_from = normalize_rel_path(raw_from)
            if norm_from != block_p1:
                raise DiffError("diff_header_mismatch", line_index=line_no, expected=block_p1, actual=norm_from)
            record_path(norm_from)
            idx += 1
            continue

        if line.startswith("rename to ") or line.startswith("copy to "):
            if not in_git_block:
                raise DiffError("diff_parse_failed", line_index=line_no, message="rename/copy outside diff --git block")
            raw_to = line.split(" ", 2)[2]
            norm_to = normalize_rel_path(raw_to)
            if norm_to != block_p2:
                raise DiffError("diff_header_mismatch", line_index=line_no, expected=block_p2, actual=norm_to)
            current_path = norm_to
            record_path(norm_to)
            idx += 1
            continue

        if line == "GIT binary patch":
            if not allow_binary:
                raise DiffError("binary_patch_not_allowed", path=current_path or "", line_index=line_no)
            saw_header = True
            idx += 1
            while idx < total:
                next_line = lines[idx]
                if next_line.startswith("diff --git ") or next_line.startswith("--- "):
                    break
                idx += 1
            continue

        bin_match = BINARY_FILES_RE.match(line)
        if bin_match:
            left_raw, right_raw = bin_match.group(1), bin_match.group(2)
            left_path = parse_binary_side(left_raw, "a/")
            right_path = parse_binary_side(right_raw, "b/")
            if in_git_block:
                if left_path is not None and left_path != block_p1:
                    raise DiffError("diff_header_mismatch", line_index=line_no, expected=block_p1, actual=left_path)
                if right_path is not None and right_path != block_p2:
                    raise DiffError("diff_header_mismatch", line_index=line_no, expected=block_p2, actual=right_path)
            if not allow_binary:
                raise DiffError(
                    "binary_patch_not_allowed",
                    path=right_path or left_path or current_path or "",
                    line_index=line_no,
                )
            saw_header = True
            if left_path is not None:
                record_path(left_path)
            if right_path is not None:
                record_path(right_path)
                current_path = right_path
            idx += 1
            continue

        if line.startswith("--- "):
            raw_old = strip_header_timestamp(line[4:])
            if in_git_block:
                if raw_old != "/dev/null":
                    if raw_old.startswith('"') or raw_old.endswith('"'):
                        raise DiffError("unsafe_path", line_index=line_no, message="quoted --- path is not supported")
                    if not raw_old.startswith("a/"):
                        raise DiffError("diff_header_mismatch", line_index=line_no, expected=block_p1, actual=raw_old)
                    norm_old = normalize_rel_path(raw_old[2:])
                    if norm_old != block_p1:
                        raise DiffError("diff_header_mismatch", line_index=line_no, expected=block_p1, actual=norm_old)
                    record_path(norm_old)
                if idx + 1 >= total or not lines[idx + 1].startswith("+++ "):
                    raise DiffError("diff_parse_failed", line_index=line_no, message="--- header missing matching +++")
                raw_new = strip_header_timestamp(lines[idx + 1][4:])
                if raw_new != "/dev/null":
                    if raw_new.startswith('"') or raw_new.endswith('"'):
                        raise DiffError("unsafe_path", line_index=line_no + 1, message="quoted +++ path is not supported")
                    if not raw_new.startswith("b/"):
                        raise DiffError("diff_header_mismatch", line_index=line_no + 1, expected=block_p2, actual=raw_new)
                    norm_new = normalize_rel_path(raw_new[2:])
                    if norm_new != block_p2:
                        raise DiffError("diff_header_mismatch", line_index=line_no + 1, expected=block_p2, actual=norm_new)
                    current_path = norm_new
                    record_path(norm_new)
                idx += 2
                continue
            else:
                if idx + 1 >= total or not lines[idx + 1].startswith("+++ "):
                    raise DiffError("diff_parse_failed", line_index=line_no, message="--- header missing matching +++")
                raw_new = strip_header_timestamp(lines[idx + 1][4:])
                use_ab = (
                    (raw_old == "/dev/null" or raw_old.startswith("a/"))
                    and (raw_new == "/dev/null" or raw_new.startswith("b/"))
                    and not (raw_old == "/dev/null" and raw_new == "/dev/null")
                )
                norm_old = None
                norm_new = None
                if raw_old != "/dev/null":
                    norm_old = normalize_rel_path(raw_old[2:] if use_ab and raw_old.startswith("a/") else raw_old)
                    record_path(norm_old)
                if raw_new != "/dev/null":
                    norm_new = normalize_rel_path(raw_new[2:] if use_ab and raw_new.startswith("b/") else raw_new)
                    record_path(norm_new)
                if norm_old is None and norm_new is None:
                    raise DiffError("diff_parse_failed", line_index=line_no, message="both --- and +++ are /dev/null")
                current_path = norm_new or norm_old
                saw_header = True
                idx += 2
                continue

        hunk_match = HUNK_RE.match(line)
        if hunk_match:
            if not saw_header:
                raise DiffError("diff_parse_failed", line_index=line_no, message="hunk before file header")
            old_rem = int(hunk_match.group(2)) if hunk_match.group(2) is not None else 1
            new_rem = int(hunk_match.group(4)) if hunk_match.group(4) is not None else 1
            in_hunk = True
            idx += 1
            continue

        if (
            line == ""
            or line.startswith("index ")
            or line.startswith("similarity index ")
            or line.startswith("dissimilarity index ")
        ):
            idx += 1
            continue

        if in_git_block and line.startswith("+++ "):
            raise DiffError("diff_parse_failed", line_index=line_no, message="unexpected +++ without ---")

        raise DiffError("diff_parse_failed", line_index=line_no, message="unrecognized diff line")

    if in_hunk and (old_rem > 0 or new_rem > 0):
        raise DiffError("diff_parse_failed", message="truncated hunk at end of diff")

    return touched, added_lines, removed_lines


def parse_begin_patch(lines, reject_secret_patterns):
    non_empty = [(idx + 1, line) for idx, line in enumerate(lines) if line != ""]
    if not non_empty:
        return [], 0, 0
    if non_empty[0][1] != "*** Begin Patch" or non_empty[-1][1] != "*** End Patch":
        raise DiffError("diff_parse_failed", message="Begin Patch must start with '*** Begin Patch' and end with '*** End Patch'")

    start_idx = non_empty[0][0] - 1
    end_idx = non_empty[-1][0] - 1

    touched = []
    touched_set = set()
    added_lines = 0
    removed_lines = 0
    mode = None
    current_path = None

    def record_path(path):
        if path not in touched_set:
            touched_set.add(path)
            touched.append(path)

    for idx in range(start_idx + 1, end_idx):
        line = lines[idx]
        line_no = idx + 1

        if line.startswith("*** Add File: "):
            path = normalize_rel_path(line[len("*** Add File: ") :])
            record_path(path)
            current_path = path
            mode = "add"
            continue
        if line.startswith("*** Update File: "):
            path = normalize_rel_path(line[len("*** Update File: ") :])
            record_path(path)
            current_path = path
            mode = "update"
            continue
        if line.startswith("*** Delete File: "):
            path = normalize_rel_path(line[len("*** Delete File: ") :])
            record_path(path)
            current_path = path
            mode = "delete"
            continue
        if line.startswith("*** Move to: "):
            if mode != "update":
                raise DiffError("diff_parse_failed", line_index=line_no, message="Move to must follow Update File")
            path = normalize_rel_path(line[len("*** Move to: ") :])
            record_path(path)
            current_path = path
            continue
        if line == "*** End of File":
            continue
        if line.startswith("*** "):
            raise DiffError("diff_parse_failed", line_index=line_no, message="unrecognized Begin Patch directive")

        if mode == "add":
            if not line.startswith("+"):
                raise DiffError("diff_parse_failed", line_index=line_no, message="Add File lines must start with '+'")
            added_lines += 1
            check_secret_line(line[1:], current_path, line_no, reject_secret_patterns)
        elif mode == "update":
            if line.startswith("@@"):
                continue
            if line.startswith("+"):
                added_lines += 1
                check_secret_line(line[1:], current_path, line_no, reject_secret_patterns)
            elif line.startswith("-"):
                removed_lines += 1
            elif line.startswith(" ") or line == "":
                continue
            else:
                raise DiffError("diff_parse_failed", line_index=line_no, message="invalid line inside Update File hunk")
        elif mode == "delete":
            if line != "":
                raise DiffError("diff_parse_failed", line_index=line_no, message="unexpected content under Delete File")
        else:
            if line != "":
                raise DiffError("diff_parse_failed", line_index=line_no, message="content before file directive")

    return touched, added_lines, removed_lines


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
    if "allowed_prefixes" not in keys or not keys.issubset(ALLOWED_PARAM_KEYS):
        fail("params_shape_invalid", keys=sorted(keys))
        return

    if "timeout_ms" in params:
        timeout_ms = params["timeout_ms"]
        if not is_strict_int(timeout_ms) or timeout_ms < 1:
            fail("params_invalid", field="timeout_ms")
            return

    allowed_prefixes = params["allowed_prefixes"]
    if (
        not isinstance(allowed_prefixes, list)
        or not allowed_prefixes
        or len(set(allowed_prefixes)) != len(allowed_prefixes)
        or any(not validate_prefix(prefix) for prefix in allowed_prefixes)
    ):
        fail("params_invalid", field="allowed_prefixes")
        return

    denied_prefixes = params.get("denied_prefixes", [])
    if (
        not isinstance(denied_prefixes, list)
        or len(set(denied_prefixes)) != len(denied_prefixes)
        or any(not validate_prefix(prefix) for prefix in denied_prefixes)
    ):
        fail("params_invalid", field="denied_prefixes")
        return

    diff_format = params.get("format", "auto")
    if not isinstance(diff_format, str) or diff_format not in VALID_FORMATS:
        fail("params_invalid", field="format")
        return

    allow_empty = params.get("allow_empty", False)
    if type(allow_empty) is not bool:
        fail("params_invalid", field="allow_empty")
        return

    allow_binary = params.get("allow_binary", False)
    if type(allow_binary) is not bool:
        fail("params_invalid", field="allow_binary")
        return

    max_files = params.get("max_files")
    if max_files is not None and (not is_strict_int(max_files) or max_files < 1):
        fail("params_invalid", field="max_files")
        return

    max_changed_lines = params.get("max_changed_lines")
    if max_changed_lines is not None and (not is_strict_int(max_changed_lines) or max_changed_lines < 1):
        fail("params_invalid", field="max_changed_lines")
        return

    reject_secret_patterns = params.get("reject_secret_patterns", True)
    if type(reject_secret_patterns) is not bool:
        fail("params_invalid", field="reject_secret_patterns")
        return

    max_payload_bytes = params.get("max_payload_bytes", DEFAULT_MAX_PAYLOAD_BYTES)
    if not is_strict_int(max_payload_bytes) or not (1 <= max_payload_bytes <= MAX_ALLOWED_PAYLOAD_BYTES):
        fail("params_invalid", field="max_payload_bytes")
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

    lines = [line.rstrip("\r") for line in payload_text.split("\n")]
    if lines and lines[-1] == "":
        lines.pop()

    if not any(line.strip() for line in lines):
        detected = "unified" if diff_format == "auto" else diff_format
        if not allow_empty:
            fail("empty_diff")
            return
        verdict(
            True,
            {
                "handler": HANDLER,
                "format": detected,
                "file_count": 0,
                "added_lines": 0,
                "removed_lines": 0,
                "touched_paths": [],
            },
        )
        return

    if diff_format == "auto":
        first_non_empty = next((line for line in lines if line != ""), "")
        detected = "begin_patch" if first_non_empty == "*** Begin Patch" else "unified"
    else:
        detected = diff_format

    try:
        if detected == "begin_patch":
            touched_paths, added_lines, removed_lines = parse_begin_patch(
                lines, reject_secret_patterns
            )
        else:
            touched_paths, added_lines, removed_lines = parse_unified(
                lines, allow_binary, reject_secret_patterns
            )
    except DiffError as err:
        fail(err.reason, **err.details)
        return

    if not touched_paths:
        if not allow_empty:
            fail("empty_diff")
            return
        verdict(
            True,
            {
                "handler": HANDLER,
                "format": detected,
                "file_count": 0,
                "added_lines": added_lines,
                "removed_lines": removed_lines,
                "touched_paths": [],
            },
        )
        return

    for path in touched_paths:
        for denied in denied_prefixes:
            if matches_prefix(path, denied):
                fail("path_denied", path=path, denied_prefix=denied)
                return
        if not any(matches_prefix(path, allowed) for allowed in allowed_prefixes):
            fail("path_out_of_scope", path=path)
            return

    if max_files is not None and len(touched_paths) > max_files:
        fail("max_files_exceeded", file_count=len(touched_paths), max_files=max_files)
        return

    changed_lines = added_lines + removed_lines
    if max_changed_lines is not None and changed_lines > max_changed_lines:
        fail(
            "max_changed_lines_exceeded",
            changed_lines=changed_lines,
            max_changed_lines=max_changed_lines,
        )
        return

    verdict(
        True,
        {
            "handler": HANDLER,
            "format": detected,
            "file_count": len(touched_paths),
            "added_lines": added_lines,
            "removed_lines": removed_lines,
            "touched_paths": touched_paths,
        },
    )


if __name__ == "__main__":
    main()
