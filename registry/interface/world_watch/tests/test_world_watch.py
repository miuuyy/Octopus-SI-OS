import hashlib
import hmac
import http.server
import importlib.util
import json
import os
import sys
import tempfile
import threading
import unittest
import urllib.error
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))

from world_watch import ProbeError, Watcher, WebhookServer, build_probe, validate_config


def load_boundary_with_env(env):
    saved = {k: os.environ.get(k) for k in env}
    os.environ.update(env)
    try:
        spec = importlib.util.spec_from_file_location("world_watch_boundary_test", ROOT / "world_watch_boundary.py")
        mod = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(mod)
        return mod
    finally:
        for k, v in saved.items():
            if v is None:
                os.environ.pop(k, None)
            else:
                os.environ[k] = v


class FakeRuntime:
    def __init__(self, boundary):
        self.b = boundary
        self.seq = 0
        self.delivered = []
        self.boundary_hand_calls = 0

    def hand(self, manifest_path, evidence):
        self.boundary_hand_calls += 1
        manifest = json.loads(Path(manifest_path).read_text(encoding="utf-8"))
        payload = json.loads(Path(manifest["payload_path"]).read_text(encoding="utf-8"))
        artifact_id = evidence[0]["artifact_id"]
        seq = self.seq
        self.seq += 1
        row = {
            "seq": seq,
            "kind": "InterfaceArrival",
            "from": {"Interface": self.b.INTERFACE_ID},
            "to": {"Interface": "system_worker_operator_task"},
            "artifact": {"id": artifact_id, "content": manifest["payload_content_hash"]},
            "recorded_at_unix_ms": 1700000000000 + seq * 1000,
        }
        with open(self.b.MOVEMENTS_JSONL, "ab") as handle:
            handle.write(json.dumps(row).encode("utf-8") + b"\n")
        self.delivered.append(payload)
        return seq


class WorldWatchContractTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        self.var_dir = self.root / "var"
        self.runs_dir = self.root / "runs"
        self.iface_dir = self.root / "world_watch_demo"
        self.var_dir.mkdir(mode=0o700)
        self.runs_dir.mkdir()
        self.iface_dir.mkdir()
        self.movements = self.root / "movements.jsonl"
        self.movements.write_bytes(b"")
        self.env = {
            "OCTOPUS_INTERFACE_ID": "world_watch_demo",
            "OCTOPUS_INTERFACE_DIR": str(self.iface_dir),
            "OCTOPUS_VAR_DIR": str(self.var_dir),
            "OCTOPUS_MOVEMENTS_JSONL": str(self.movements),
            "OCTOPUS_INTERFACE_HOST_IO_LIMITS": json.dumps(
                {
                    "max_control_frame_bytes": 1048576,
                    "max_manifest_bytes": 1048576,
                    "max_payload_bytes": 8388608,
                    "max_stderr_bytes": 262144,
                }
            ),
            "OCTOPUS_ROOT_SLOTS_JSON": json.dumps({"run_records_root": str(self.runs_dir)}),
        }
        self.boundary = load_boundary_with_env(self.env)
        self.runtime = FakeRuntime(self.boundary)
        self.boundary.hand = self.runtime.hand

    def tearDown(self):
        self.tmp.cleanup()

    def _start_http_server(self, state):
        class Handler(http.server.BaseHTTPRequestHandler):
            def do_GET(self):
                state["requests"].append(dict(self.headers))
                if state.get("etag") and self.headers.get("If-None-Match") == state["etag"]:
                    self.send_response(304)
                    self.end_headers()
                    return
                body = state["body"].encode("utf-8")
                self.send_response(state["status"])
                if state.get("etag"):
                    self.send_header("ETag", state["etag"])
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def log_message(self, *_args):
                return

        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        return server, f"http://127.0.0.1:{server.server_port}/status"

    def test_validate_config_fails_closed(self):
        base = {
            "enabled": True,
            "poll_seconds": 30,
            "source": {"kind": "http", "url": "https://example.com/health"},
            "trigger": {"mode": "on_change"},
        }
        self.assertEqual(validate_config(base)["label"], "watch.event")
        with self.assertRaises(ValueError):
            validate_config({**base, "unexpected": 1})
        with self.assertRaises(ValueError):
            validate_config({**base, "poll_seconds": 1})
        with self.assertRaises(ValueError):
            validate_config({**base, "source": {"kind": "http", "url": "file:///etc/passwd"}})
        with self.assertRaises(ValueError):
            validate_config(
                {
                    **base,
                    "source": {
                        "kind": "http",
                        "url": "https://example.com",
                        "headers": {"Authorization": "Bearer leak"},
                    },
                }
            )
        with self.assertRaises(ValueError):
            validate_config(
                {
                    **base,
                    "source": {
                        "kind": "http",
                        "url": "https://example.com",
                        "auth": {"type": "bearer", "token_file": "../escape"},
                    },
                }
            )
        with self.assertRaises(ValueError):
            validate_config({**base, "trigger": {"mode": "on_match"}})
        with self.assertRaises(ValueError):
            validate_config({**base, "trigger": {"mode": "on_change", "condition": {"equals": "x"}}})

    def test_http_on_change_etag_auth_and_reclaim(self):
        token_path = self.var_dir / "api.token"
        token_path.write_text("secret-token-123\n", encoding="utf-8")
        os.chmod(token_path, 0o600)
        state = {"status": 200, "body": json.dumps({"release": {"tag": "v1.0"}}), "etag": '"e1"', "requests": []}
        server, url = self._start_http_server(state)
        try:
            cfg = validate_config(
                {
                    "enabled": True,
                    "poll_seconds": 10,
                    "source": {
                        "kind": "http",
                        "url": url,
                        "auth": {"type": "bearer", "token_file": "api.token"},
                    },
                    "extract": {"format": "json", "json_pointer": "/release/tag"},
                    "trigger": {"mode": "on_change", "fire_on_first": False},
                    "hold_while_working": False,
                    "note": "Track release tag",
                }
            )
            watcher = Watcher(cfg, self.boundary)
            try:
                probe = build_probe(cfg, self.boundary)
                watcher.cycle(probe, now_unix=1000.0)
                self.assertEqual(len(self.runtime.delivered), 0)
                self.assertEqual(state["requests"][0].get("Authorization"), "Bearer secret-token-123")

                watcher.cycle(probe, now_unix=1010.0)
                self.assertEqual(len(self.runtime.delivered), 0)
                self.assertEqual(state["requests"][1].get("If-None-Match"), '"e1"')

                state["body"] = json.dumps({"release": {"tag": "v1.1"}})
                state["etag"] = '"e2"'
                watcher.cycle(probe, now_unix=1020.0)
                self.assertEqual(len(self.runtime.delivered), 1)
                notice = self.runtime.delivered[0]
                self.assertTrue(notice["untrusted_external_data"])
                self.assertEqual(notice["reason"], "changed")
                self.assertEqual(notice["previous_value"], "v1.0")
                self.assertEqual(notice["value"], "v1.1")
                self.assertEqual(notice["note"], "Track release tag")
                self.assertEqual(list(Path(self.boundary.PAYLOADS_DIR).iterdir()), [])
            finally:
                watcher.close()
        finally:
            server.shutdown()
            server.server_close()

    def test_on_match_edge_recovery_and_error_episode(self):
        state = {"status": 200, "body": json.dumps({"price": 120}), "etag": None, "requests": []}
        server, url = self._start_http_server(state)
        try:
            cfg = validate_config(
                {
                    "enabled": True,
                    "poll_seconds": 10,
                    "source": {"kind": "http", "url": url, "conditional": False},
                    "extract": {"format": "json", "json_pointer": "/price"},
                    "trigger": {
                        "mode": "on_match",
                        "condition": {"lt": 100},
                        "edge_triggered": True,
                        "notify_on_recovery": True,
                        "notify_on_error": True,
                    },
                    "hold_while_working": False,
                }
            )
            watcher = Watcher(cfg, self.boundary)
            try:
                probe = build_probe(cfg, self.boundary)
                watcher.cycle(probe, now_unix=100.0)
                self.assertEqual(len(self.runtime.delivered), 0)

                state["body"] = json.dumps({"price": 85})
                watcher.cycle(probe, now_unix=110.0)
                watcher.cycle(probe, now_unix=120.0)
                self.assertEqual([d["reason"] for d in self.runtime.delivered], ["matched"])

                state["body"] = json.dumps({"price": 115})
                watcher.cycle(probe, now_unix=130.0)
                self.assertEqual([d["reason"] for d in self.runtime.delivered], ["matched", "recovered"])

                state["body"] = "not-json"
                watcher.cycle(probe, now_unix=140.0)
                watcher.cycle(probe, now_unix=150.0)
                self.assertEqual(
                    [d["reason"] for d in self.runtime.delivered],
                    ["matched", "recovered", "probe_error"],
                )
                self.assertEqual(self.runtime.delivered[-1]["error_code"], "invalid_json")

                state["body"] = json.dumps({"price": 115})
                watcher.cycle(probe, now_unix=160.0)
                self.assertEqual(
                    [d["reason"] for d in self.runtime.delivered],
                    ["matched", "recovered", "probe_error", "error_recovered"],
                )
            finally:
                watcher.close()
        finally:
            server.shutdown()
            server.server_close()

    def test_file_watch_hold_while_working_and_idempotent_recovery(self):
        watched = self.root / "status.txt"
        run_path = self.runs_dir / "run_target.json"
        run_record = {
            "id": "run_target",
            "ee": "ee_root",
            "worker": "system_worker_operator_task",
            "trace": "trace_1",
            "status": "running",
            "checkpoints": [
                {
                    "id": "cp_watch",
                    "run": "run_target",
                    "trace": "trace_1",
                    "epoch": 0,
                    "interface_binding": {
                        "interface": "world_watch_demo",
                        "config": {"artifact_policy": {"mode": "all"}},
                    },
                }
            ],
            "active_checkpoints": ["cp_watch"],
        }
        run_path.write_text(json.dumps(run_record), encoding="utf-8")

        cfg = validate_config(
            {
                "enabled": True,
                "poll_seconds": 10,
                "source": {"kind": "file", "path": str(watched), "missing_ok": True},
                "extract": {"format": "text", "strip": True},
                "trigger": {"mode": "on_change", "fire_on_first": False},
                "hold_while_working": True,
            }
        )
        saved_slots = os.environ.get("OCTOPUS_ROOT_SLOTS_JSON")
        os.environ["OCTOPUS_ROOT_SLOTS_JSON"] = json.dumps({"run_records_root": str(self.runs_dir)})
        try:
            watcher = Watcher(cfg, self.boundary)
            try:
                probe = build_probe(cfg, self.boundary)
                watcher.cycle(probe, now_unix=200.0)
                self.assertEqual(len(self.runtime.delivered), 0)

                watched.write_text("deploy-ready\n", encoding="utf-8")
                delay = watcher.cycle(probe, now_unix=210.0)
                self.assertEqual(delay, 1.0)
                self.assertEqual(len(self.runtime.delivered), 0)
                self.assertEqual(watcher.health["state"], "holding")
                self.assertEqual(watcher.health["held_by"], ["run_target"])

                run_record["status"] = "idle"
                run_path.write_text(json.dumps(run_record), encoding="utf-8")
                watcher.cycle(probe, now_unix=211.0)
                self.assertEqual(len(self.runtime.delivered), 1)
                self.assertEqual(self.runtime.delivered[0]["value"], "deploy-ready")
                self.assertEqual(self.runtime.boundary_hand_calls, 1)

                watched.write_text("deploy-done\n", encoding="utf-8")
                watcher._prepare_observation(probe.fetch(None), "2026-11-23T06:00:00Z", 220.0)
                pending = watcher.read("pending")
                notice = pending["notices"][0]
                self.runtime.hand(
                    notice["manifest_path"],
                    [{"artifact_id": f"{notice['label']}:{notice['manifest_hash']}"}],
                )
                calls_before = self.runtime.boundary_hand_calls
                self.assertEqual(watcher.deliver(), "delivered")
                self.assertEqual(self.runtime.boundary_hand_calls, calls_before)
                self.assertIsNone(watcher.read("pending"))
            finally:
                watcher.close()
        finally:
            if saved_slots is None:
                os.environ.pop("OCTOPUS_ROOT_SLOTS_JSON", None)
            else:
                os.environ["OCTOPUS_ROOT_SLOTS_JSON"] = saved_slots

    def test_webhook_hmac_validation_queuing_and_delivery(self):
        secret_path = self.var_dir / "hook.secret"
        secret_path.write_text("top-secret-key\n", encoding="utf-8")
        os.chmod(secret_path, 0o600)
        cfg = validate_config(
            {
                "enabled": True,
                "source": {
                    "kind": "webhook",
                    "bind": "127.0.0.1",
                    "port": 0,
                    "path": "/events",
                    "methods": ["POST"],
                    "auth": {
                        "type": "hmac_sha256",
                        "header": "X-Hub-Signature-256",
                        "prefix": "sha256=",
                        "token_file": "hook.secret",
                    },
                },
                "extract": {"format": "json", "json_pointer": "/event"},
                "trigger": {"mode": "always"},
                "hold_while_working": False,
                "max_response_bytes": 1024,
            }
        )
        watcher = Watcher(cfg, self.boundary)
        wake = threading.Event()
        server = WebhookServer(cfg, watcher, self.boundary, wake_event=wake)
        server.start()
        base_url = f"http://127.0.0.1:{server.port}"
        try:
            def post(path, body, sig=None):
                headers = {"Content-Type": "application/json"}
                if sig is not None:
                    headers["X-Hub-Signature-256"] = sig
                req = urllib.request.Request(base_url + path, data=body, headers=headers, method="POST")
                try:
                    with urllib.request.urlopen(req, timeout=5) as resp:
                        return resp.status, json.loads(resp.read().decode("utf-8"))
                except urllib.error.HTTPError as err:
                    body_out = json.loads(err.read().decode("utf-8"))
                    err.close()
                    return err.code, body_out

            status, _ = post("/wrong", b'{"event":"x"}')
            self.assertEqual(status, 404)
            status, _ = post("/events", b'{"event":"x"}', sig="sha256=deadbeef")
            self.assertEqual(status, 401)
            status, _ = post("/events", b"x" * 2000, sig="sha256=00")
            self.assertEqual(status, 413)

            for ev in ("build.done", "deploy.done"):
                payload = json.dumps({"event": ev}).encode("utf-8")
                sig = "sha256=" + hmac.new(b"top-secret-key", payload, hashlib.sha256).hexdigest()
                status, body = post("/events", payload, sig=sig)
                self.assertEqual(status, 202)
                self.assertTrue(body["triggered"])
                self.assertEqual(body["reason"], "push")

            self.assertTrue(wake.is_set())
            self.assertEqual(watcher.deliver(), "delivered")
            self.assertEqual([d["value"] for d in self.runtime.delivered], ["build.done", "deploy.done"])
            self.assertTrue(all(d["reason"] == "push" and d["untrusted_external_data"] for d in self.runtime.delivered))
            self.assertEqual(list(Path(self.boundary.PAYLOADS_DIR).iterdir()), [])
        finally:
            server.stop()
            watcher.close()


if __name__ == "__main__":
    unittest.main()
