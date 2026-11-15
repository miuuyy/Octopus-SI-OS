#!/usr/bin/env python3
"""GitHub polling sensor over the declared interface life protocol."""
import fcntl
import importlib.util
import json
import os
import signal
import sys
import threading
import time
from pathlib import Path

from github_api import API, PollError, validate_config
from github_watch import Watcher

CONFIG_NAME = "github-watch.json"
MAX_CONFIG_BYTES = 65536


def load_boundary(path):
    path = Path(path)
    sys.path.insert(0, str(path.parent))
    spec = importlib.util.spec_from_file_location("github_watch_boundary", path)
    boundary = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(boundary)
    return boundary


def load_config(boundary):
    """Read the watch configuration, private state first.

    The registry folder holds the package's template and is rewritten by a
    package repair or a rebuild; a copy in the private state directory
    survives both. Only an absent private file falls back to the registry
    folder: an unusable one is an error, never a silent fallback.
    """
    try:
        raw = boundary.stable_read(Path(boundary.VAR_DIR) / CONFIG_NAME, MAX_CONFIG_BYTES, "private github watch config")
        source = "private_state"
    except FileNotFoundError:
        raw = boundary.stable_read(Path(boundary.INTERFACE_DIR) / CONFIG_NAME, MAX_CONFIG_BYTES, "github watch config")
        source = "registry"
    return validate_config(json.loads(raw)), source


def main():
    if len(sys.argv) != 2:
        raise ValueError("expected explicit world_legacy_watch boundary import")
    os.umask(0o077)
    b = load_boundary(sys.argv[1])
    b.private_directory(b.VAR_DIR)
    lock = os.open(str(Path(b.VAR_DIR) / "watch.lock"),
                   os.O_CREAT | os.O_RDWR | getattr(os, "O_NOFOLLOW", 0), 0o600)
    fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
    c, _source = load_config(b)
    watcher = Watcher(c, b)
    stop = threading.Event()
    for sig in (signal.SIGINT, signal.SIGTERM):
        signal.signal(sig, lambda *_: stop.set())
    b.signal_ready()
    try:
        # Preserve rate-limit backoff across process restarts.
        stop.wait(max(0, watcher.health.get("next_poll_at", 0) - time.time()))
        while not stop.is_set():
            try:
                raw = b.stable_read(str(Path(b.VAR_DIR) / "github.token"), 4096,
                                    "GitHub credential", private=True)
                token = raw.decode("ascii").strip()
                if not token or any(ord(char) < 33 or ord(char) > 126 for char in token):
                    raise ValueError()
            except (OSError, ValueError, UnicodeDecodeError):
                watcher.status("auth_unavailable", error="private_github_token_unavailable")
                stop.wait(c["poll_seconds"])
                continue
            delay = watcher.cycle(API(c, token))
            stop.wait(max(c["poll_seconds"], delay))
    except Exception:
        # A malformed state or uncertain protocol is not a successful poll.
        watcher.status("failed", error="watch_boundary_failed",
                       pending=watcher.read("pending") is not None)
        raise RuntimeError("github watcher stopped; inspect private health.json; pending work retained") from None
    finally:
        watcher.close()
        os.close(lock)


if __name__ == "__main__":
    main()
