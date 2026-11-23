#!/usr/bin/env python3
"""JSON-configured polling sensor over the declared interface life protocol."""
import fcntl
import importlib.util
import json
import os
import signal
import sys
import threading
import time
from pathlib import Path

from world_watch import Watcher, build_probe, validate_config


DISABLED_POLL_SECONDS = 5.0


def load_boundary(path):
    path = Path(path)
    sys.path.insert(0, str(path.parent))
    spec = importlib.util.spec_from_file_location("world_watch_boundary", path)
    boundary = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(boundary)
    return boundary


def read_probe_config(boundary):
    config_path = Path(boundary.INTERFACE_DIR) / "probe.json"
    info = os.lstat(config_path)
    fingerprint = (info.st_dev, info.st_ino, info.st_size, info.st_mtime_ns)
    raw = json.loads(boundary.stable_read(str(config_path), 65536, "world watch config"))
    return fingerprint, validate_config(raw)


def main():
    if len(sys.argv) != 1:
        raise ValueError("world watch life takes no arguments")
    os.umask(0o077)
    boundary = load_boundary(Path(__file__).resolve().with_name("world_watch_boundary.py"))
    boundary.private_directory(boundary.VAR_DIR)
    lock = os.open(
        str(Path(boundary.VAR_DIR) / "watch.lock"),
        os.O_CREAT | os.O_RDWR | getattr(os, "O_NOFOLLOW", 0),
        0o600,
    )
    fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
    fingerprint, config = read_probe_config(boundary)
    watcher = Watcher(config, boundary)
    stop = threading.Event()
    for sig in (signal.SIGINT, signal.SIGTERM):
        signal.signal(sig, lambda *_: stop.set())
    boundary.signal_ready()
    try:
        while not stop.is_set():
            try:
                config_path = Path(boundary.INTERFACE_DIR) / "probe.json"
                info = os.lstat(config_path)
                current_fp = (info.st_dev, info.st_ino, info.st_size, info.st_mtime_ns)
                if current_fp != fingerprint:
                    fingerprint, config = read_probe_config(boundary)
                    watcher.reconfigure(config)
            except (OSError, ValueError, json.JSONDecodeError) as error:
                watcher.status("invalid_config", error=str(error)[:512])
                stop.wait(DISABLED_POLL_SECONDS)
                continue
            if not config["enabled"]:
                watcher.status("disabled", error=None, pending=watcher.read("pending") is not None)
                stop.wait(DISABLED_POLL_SECONDS)
                continue
            delay = watcher.cycle(build_probe(config, boundary))
            stop.wait(max(0.5, delay))
    except Exception:
        watcher.status(
            "failed",
            error="watch_boundary_failed",
            pending=watcher.read("pending") is not None,
        )
        raise RuntimeError("world watcher stopped; inspect private health.json; pending work retained") from None
    finally:
        watcher.close()
        os.close(lock)


if __name__ == "__main__":
    main()
