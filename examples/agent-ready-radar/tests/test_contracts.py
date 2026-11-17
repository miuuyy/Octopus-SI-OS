import base64
import hashlib
import http.server
import json
import os
from pathlib import Path
import subprocess
import sys
import threading
import time
import unittest
from unittest.mock import patch
import urllib.error
import ssl

SOURCE = str(Path(__file__).resolve().parents[1] / "src")
sys.path.insert(0, SOURCE)

from agent_ready_radar.core import (probe, classify, scan, Spacing, statistics,
                                    report, card, origin, robots_stances, BOTS)

MD = "# Example agent documentation\n\nUseful instructions and reference links.\n"
HTML = "<!DOCTYPE html><html><body>Our app home</body></html>"


class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_GET(self):
        route = self.path
        if route == "/timeout":
            time.sleep(.25)
        status, mime, body = 200, "text/plain", MD
        if route in ("/spa/llms.txt", "/spa/.well-known/mcp.json"):
            body = HTML
        elif route == "/.well-known/mcp":
            self.send_response(302)
            self.send_header("Location", "/")
            self.end_headers()
            return
        elif route == "/":
            mime, body = "text/html", HTML
        elif route == "/.well-known/mcp.json":
            mime, body = "application/json", '{"transport":"streamable-http","url":"https://example.org/mcp"}'
        elif route == "/bad-json":
            mime, body = "application/json", "{"
        elif route == "/oversize":
            body = MD + "x"*10000
        elif route == "/robots.txt":
            body = "User-agent: GPTBot\nDisallow: /\nUser-agent: ClaudeBot\nAllow: /\nUser-agent: *\nDisallow: /private\n"
        elif route == "/missing":
            status, body = 404, "Not found"
        elif route == "/blocked":
            status, body = 403, "Forbidden"
        elif route == "/loop":
            self.send_response(301)
            self.send_header("Location", "/loop")
            self.end_headers()
            return
        self.send_response(status)
        self.send_header("Content-Type", mime)
        self.send_header("Content-Length", str(len(body.encode())))
        self.end_headers()
        try:
            self.wfile.write(body.encode())
        except (BrokenPipeError, ConnectionResetError):
            pass


def validate(value, schema, root):
    """Small fail-closed validator for the schema keywords reached by our native card.
    Not a general JSON Schema library; validates against the saved live client schema.
    """
    if "$ref" in schema:
        ref = schema["$ref"]
        assert ref.startswith("#/"), "External ref not supported by this fixture validator"
        target = root
        for key in ref[2:].split("/"):
            target = target[key]
        validate(value, target, root)
    if "oneOf" in schema:
        valid = 0
        for branch in schema["oneOf"]:
            try:
                validate(value, branch, root)
                valid += 1
            except AssertionError:
                pass
        assert valid == 1, ("oneOf", valid)
    for branch in schema.get("allOf", []):
        validate(value, branch, root)
    if "if" in schema:
        try:
            validate(value, schema["if"], root)
            condition = True
        except AssertionError:
            condition = False
        validate(value, schema.get("then" if condition else "else", {}), root)
    types = {"object": lambda x: isinstance(x, dict), "array": lambda x: isinstance(x, list),
             "string": lambda x: isinstance(x, str), "integer": lambda x: type(x) is int,
             "number": lambda x: type(x) in (int, float), "boolean": lambda x: type(x) is bool,
             "null": lambda x: x is None}
    if "type" in schema:
        ts = schema["type"] if isinstance(schema["type"], list) else [schema["type"]]
        assert any(types[t](value) for t in ts), ("type", value)
    if "const" in schema:
        assert value == schema["const"]
    if "enum" in schema:
        assert value in schema["enum"]
    if isinstance(value, dict):
        assert all(k in value for k in schema.get("required", []))
        props = schema.get("properties", {})
        if schema.get("additionalProperties") is False:
            assert set(value) <= set(props), set(value) - set(props)
        for k, v in value.items():
            if k in props:
                validate(v, props[k], root)
    if isinstance(value, list):
        assert schema.get("minItems", 0) <= len(value) <= schema.get("maxItems", float("inf"))
        if "items" in schema:
            for item in value:
                validate(item, schema["items"], root)
    if isinstance(value, str):
        import re
        assert schema.get("minLength", 0) <= len(value) <= schema.get("maxLength", float("inf"))
        if "pattern" in schema:
            assert re.search(schema["pattern"], value)
    if type(value) in (int, float):
        assert schema.get("minimum", -float("inf")) <= value <= schema.get("maximum", float("inf"))


class Contracts(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        cls.thread = threading.Thread(target=cls.server.serve_forever, daemon=True)
        cls.thread.start()
        cls.base = "http://127.0.0.1:" + str(cls.server.server_port)

    @classmethod
    def tearDownClass(cls):
        cls.server.shutdown()
        cls.server.server_close()
        cls.thread.join()

    def get(self, path, **kw):
        return probe(self.base, path, Spacing(0), **kw)

    def test_valid_markdown_and_mcp(self):
        for path in ("/llms.txt", "/agents.md", "/.well-known/mcp.json"):
            with self.subTest(path=path):
                self.assertTrue(self.get(path)["verified"])

    def test_spa_fools_naive_scanner_not_oracle(self):
        for path in ("/spa/llms.txt", "/spa/.well-known/mcp.json"):
            p = self.get(path)
            self.assertEqual(p["status"], 200)  # naive scanner counts it!
            self.assertFalse(p["verified"])
            self.assertEqual(p["classification"], "soft_404_html")

    def test_redirect_home(self):
        p = self.get("/.well-known/mcp")
        self.assertEqual([h["status"] for h in p["redirect_chain"]], [302, 200])
        self.assertEqual(p["final_url"], self.base + "/")
        self.assertEqual(p["classification"], "redirected_to_home")
        self.assertFalse(p["verified"])

    def test_real_robots(self):
        p = self.get("/robots.txt")
        self.assertTrue(p["verified"])
        bots = p["evidence"]["bots"]
        self.assertEqual(bots["GPTBot"]["stance"], "disallow")
        self.assertEqual(bots["ClaudeBot"]["stance"], "allow")
        self.assertEqual(bots["CCBot"]["stance"], "unspecified")
        self.assertEqual(len(bots), len(BOTS))

    def test_robots_html(self):
        label, _ = classify("/robots.txt", 200, self.base+"/robots.txt", "text/plain", HTML)
        self.assertEqual(label, "soft_404_html")

    def test_json_adversaries(self):
        for body, expected in (("{", "invalid_json"), ('{"error":"missing"}', "unrecognized_json"),
                               ('{"status":"ok"}', "unrecognized_json"), ("[]", "unrecognized_json"),
                               ('{"transport":"stdio","command":"server"}', "unrecognized_json")):
            self.assertEqual(classify("/.well-known/mcp.json", 200, self.base, "application/json", body)[0], expected)
        p = self.get("/bad-json")
        self.assertEqual(classify("/.well-known/mcp.json", p["status"], p["final_url"], p["content_type"], p["body"])[0], "invalid_json")

    def test_plugin_manifest(self):
        value = dict(schema_version="v1", name_for_human="Test", name_for_model="test",
                     description_for_human="Test", description_for_model="Test",
                     auth={"type": "none"}, api={"type": "openapi", "url": "https://example.org/openapi.json"})
        args = ("/.well-known/ai-plugin.json", 200, self.base, "application/json")
        self.assertEqual(classify(*args, json.dumps(value))[0], "verified_json_descriptor")
        del value["auth"]
        self.assertEqual(classify(*args, json.dumps(value))[0], "unrecognized_json")

    def test_oversize(self):
        p = self.get("/oversize", max_bytes=128)
        self.assertTrue(p["truncated"])
        self.assertEqual(p["bytes_read"], 128)
        self.assertFalse(p["verified"])
        self.assertEqual(p["classification"], "truncated_unverified")

    def test_markdown_error_is_not_artifact(self):
        label, _ = classify("/agents.md", 200, self.base, "text/plain",
                            "# Not found\n\nThe document you requested does not exist.")
        self.assertEqual(label, "unstructured_text")

    def test_robots_no_rule_group_does_not_inherit_next(self):
        bots = robots_stances("User-agent: GPTBot\nCrawl-delay: 5\nUser-agent: CCBot\nDisallow: /", self.base)
        self.assertEqual(bots["GPTBot"]["stance"], "unspecified")
        self.assertEqual(bots["CCBot"]["stance"], "disallow")

    def test_body_audit(self):
        p = self.get("/llms.txt")
        raw = base64.b64decode(p["body_base64"])
        self.assertEqual(p["body_sha256"], hashlib.sha256(raw).hexdigest())
        self.assertEqual(p["body"], raw.decode())
        self.assertGreaterEqual(p["latency_ms"], 0)

    def test_timeout(self):
        self.assertEqual(self.get("/timeout", timeout=.05)["classification"], "timeout")

    def test_tls_error(self):
        with patch("urllib.request.OpenerDirector.open", side_effect=urllib.error.URLError(ssl.SSLError("bad cert"))):
            self.assertEqual(self.get("/llms.txt")["classification"], "tls_error")

    def test_http_errors_and_loop(self):
        self.assertEqual(self.get("/missing")["classification"], "http_404")
        self.assertEqual(self.get("/blocked")["classification"], "blocked_waf")
        self.assertFalse(self.get("/loop", max_redirects=2)["verified"])

    def test_unsafe_targets(self):
        for url in ("file:///etc/passwd", "https://user:secret@example.org"):
            with self.assertRaises(ValueError):
                origin(url)

    def test_login_parked(self):
        for url, expected in (("https://example.org/login", "login_wall"),
                              ("https://sedoparking.com/domain", "parked_redirect")):
            self.assertEqual(classify("/agents.md", 200, url, "text/html", HTML, redirected=True)[0], expected)

    def test_spacing(self):
        s = Spacing(.025)
        then = time.monotonic()
        s.wait(self.base)
        s.wait(self.base)
        self.assertGreaterEqual(time.monotonic()-then, .023)

    def test_report_card_contract_and_counts(self):
        data = scan([self.base], workers=1, spacing=0)
        self.assertEqual(report(data), report(data))
        self.assertEqual(statistics(data)["domains"], 1)
        schema = json.loads((Path(__file__).parent/"client_surface.schema.json").read_text())
        doc = card(data)
        validate(doc, schema, schema)
        doc["target"]["kind"] = "invalid"
        with self.assertRaises(AssertionError):
            validate(doc, schema, schema)

    def test_cli_end_to_end(self):
        completed = subprocess.run([sys.executable, "-m", "agent_ready_radar", "scan",
                                    self.base, "--spacing", "0"], capture_output=True, text=True,
                                   env={**os.environ, "PYTHONPATH": SOURCE})
        self.assertEqual(completed.returncode, 0, completed.stderr)
        self.assertEqual(len(json.loads(completed.stdout)["domains"]), 1)


if __name__ == "__main__":
    unittest.main()
