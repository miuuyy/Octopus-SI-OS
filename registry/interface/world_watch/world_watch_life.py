#!/usr/bin/env python3
"""JSON-configured polling and webhook sensor over the declared interface life protocol."""
import fcntl
import importlib.util
import json
import os
import signal
import sys
import threading
from pathlib import Path

from world_watch import HOLD_RETRY_SECONDS, Watcher, WebhookServer, build_probe, validate_config


DISABLED_POLL_SECONDS = 5.0
CONFIG_RECHECK_SECONDS = 2.0


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


def _webhook_bind_key(config):
    source = config["source"]
    return (source["bind"], source["port"])


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
    wake = threading.Event()

    def request_stop(*_):
        stop.set()
        wake.set()

    for sig in (signal.SIGINT, signal.SIGTERM):
        signal.signal(sig, request_stop)
    boundary.signal_ready()
    webhook_server = None
    webhook_key = None

    def stop_webhook():
        nonlocal webhook_server, webhook_key
        if webhook_server is not None:
            webhook_server.stop()
            webhook_server = None
            webhook_key = None

    try:
        while not stop.is_set():
            try:
                config_path = Path(boundary.INTERFACE_DIR) / "probe.json"
                info = os.lstat(config_path)
                current_fp = (info.st_dev, info.st_ino, info.st_size, info.st_mtime_ns)
                if current_fp != fingerprint:
                    fingerprint, config = read_probe_config(boundary)
                    watcher.reconfigure(config)
                    if webhook_server is not None:
                        if not config["enabled"] or config["source"]["kind"] != "webhook" or _webhook_bind_key(config) != webhook_key:
                            stop_webhook()
                        else:
                            webhook_server.c = config
            except (OSError, ValueError, json.JSONDecodeError) as error:
                stop_webhook()
                watcher.status("invalid_config", error=str(error)[:512])
                stop.wait(DISABLED_POLL_SECONDS)
                continue
            if not config["enabled"]:
                stop_webhook()
                watcher.status("disabled", error=None, pending=watcher.read("pending") is not None)
                stop.wait(DISABLED_POLL_SECONDS)
                continue
            if config["source"]["kind"] == "webhook":
                if webhook_server is None:
                    webhook_server = WebhookServer(config, watcher, boundary, wake_event=wake)
                    webhook_server.start()
                    webhook_key = _webhook_bind_key(config)
                    watcher.status(
                        "listening",
                        error=None,
                        bind=config["source"]["bind"],
                        port=webhook_server.port,
                        path=config["source"]["path"],
                        pending=watcher.read("pending") is not None,
                    )
                wake.clear()
                delivery = watcher.deliver()
                wait_seconds = HOLD_RETRY_SECONDS if delivery == "held" else CONFIG_RECHECK_SECONDS
                if delivery == "refused":
                    wait_seconds = min(float(config["poll_seconds"]), CONFIG_RECHECK_SECONDS)
                wake.wait(wait_seconds)
            else:
                stop_webhook()
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
        stop_webhook()
        watcher.close()
        os.close(lock)


if __name__ == "__main__":
    main()
