"""Bounded HTTP observations and conservative surface verification (not protocol conformance)."""
from __future__ import annotations

import base64
import concurrent.futures
import datetime as dt
import hashlib
import json
import re
import socket
import ssl
import threading
import time
import urllib.error
import urllib.parse
import urllib.request

VERSION = "0.1.0"
USER_AGENT = "agent-ready-radar/0.1.0 (+https://github.com/octopus-sios/octopus-os; empirical readiness research)"
PATHS = ("/llms.txt", "/llms-full.txt", "/agents.md", "/.well-known/mcp.json",
         "/.well-known/mcp", "/.well-known/ai-plugin.json", "/robots.txt")
BOTS = ("GPTBot", "OAI-SearchBot", "ChatGPT-User", "ClaudeBot", "Claude-Web",
        "anthropic-ai", "Google-Extended", "PerplexityBot", "CCBot", "Bytespider")
VERIFIED = {"verified_markdown_text", "verified_json_descriptor", "verified_robots"}
ILLUSION_PATHS = {"/llms.txt", "/agents.md", "/.well-known/mcp.json", "/.well-known/mcp"}


def origin(value):
    u = urllib.parse.urlsplit(value if "://" in value else "https://" + value)
    if u.scheme not in ("http", "https") or not u.hostname or u.username or u.password:
        raise ValueError("Expected credential-free HTTP(S) URL: " + value)
    return urllib.parse.urlunsplit((u.scheme, u.netloc, "", "", ""))


class Spacing:
    """Shared across probes and redirects, keyed by hostname (including aliases once known)."""
    def __init__(self, seconds):
        self.seconds, self.lock, self.next = seconds, threading.Lock(), {}

    def wait(self, url):
        host = urllib.parse.urlsplit(url).hostname
        with self.lock:
            now = time.monotonic()
            delay = max(0, self.next.get(host, now) - now)
            self.next[host] = now + delay + self.seconds
        time.sleep(delay)


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


def robots_stances(text, url):
    groups, agents, rules = [], [], []
    has_directive = False
    for raw in text.splitlines():
        line = raw.split("#", 1)[0].strip()
        if ":" not in line:
            continue
        key, value = (s.strip() for s in line.split(":", 1))
        key = key.lower()
        if key == "user-agent":
            if has_directive:
                groups.append((agents, rules))
                agents, rules = [], []
                has_directive = False
            agents.append(value)
        elif key in ("allow", "disallow") and agents:
            rules.append((key, value))
            has_directive = True
        elif agents:
            has_directive = True
    if agents:
        groups.append((agents, rules))
    result = {}
    for bot in BOTS:
        explicit = [rule for names, rule in groups if any(n.lower() == bot.lower() for n in names)]
        merged = [r for group in explicit for r in group]
        allows = [v for k, v in merged if k == "allow"]
        denies = [v for k, v in merged if k == "disallow" and v]
        stance = ("mixed" if allows and denies else "disallow" if denies else
                  "allow" if merged else "unspecified")
        result[bot] = {"explicit": bool(explicit), "stance": stance,
                       "allow": allows, "disallow": denies}
    return result


def classify(path, status, final_url, content_type, text, truncated=False, redirected=False):
    """Return classification and evidence; verified is deliberately a heuristic label."""
    if status == 404:
        return "http_404", {}
    if status in (401, 403, 429) or re.search(
            r"cf-chl-|just a moment|verify you are human|attention required.*cloudflare", text[:8000], re.I):
        return "blocked_waf", {}
    if status != 200:
        return "http_error", {}
    final = urllib.parse.urlsplit(final_url)
    if redirected and final.path in ("", "/"):
        return "redirected_to_home", {}
    if redirected and re.search(r"/(login|signin|sign-in|auth)(/|$)", final.path, re.I):
        return "login_wall", {}
    if redirected and re.search(r"sedoparking|hugedomains|parkingcrew", final.netloc, re.I):
        return "parked_redirect", {}
    if "html" in content_type.lower() or re.search(r"<!doctype\s+html|<html\b|<head\b|<body\b", text, re.I):
        return "soft_404_html", {}
    if truncated:
        return "truncated_unverified", {}
    if path == "/robots.txt":
        if not re.search(r"^\s*user-agent\s*:", text, re.I | re.M):
            return "invalid_robots", {}
        return "verified_robots", {"bots": robots_stances(text, final_url)}
    if path.startswith("/.well-known/"):
        try:
            value = json.loads(text)
        except (ValueError, RecursionError):
            return "invalid_json", {}
        if not isinstance(value, dict) or any(k in value for k in ("error", "errors")):
            return "unrecognized_json", {}
        if path.endswith("ai-plugin.json"):
            auth, api = value.get("auth"), value.get("api")
            valid = (value.get("schema_version") == "v1" and
                     all(isinstance(value.get(k), str) and value[k].strip()
                         for k in ("name_for_human", "name_for_model", "description_for_human", "description_for_model")) and
                     isinstance(auth, dict) and isinstance(auth.get("type"), str) and
                     isinstance(api, dict) and api.get("type") == "openapi" and
                     isinstance(api.get("url"), str) and api["url"].startswith(("http://", "https://")))
            rule = "openai_plugin_v1_core"
        else:
            def server(v):
                if not isinstance(v, dict):
                    return False
                transport = v.get("transport")
                endpoint = v.get("url") or v.get("endpoint")
                if isinstance(transport, dict):
                    endpoint = transport.get("url") or transport.get("endpoint") or endpoint
                    transport = transport.get("type")
                return (transport in ("http", "streamable-http", "sse") and
                        isinstance(endpoint, str) and endpoint.startswith(("https://", "http://")))
            servers = value.get("mcpServers") or value.get("servers")
            candidates = list(servers.values()) if isinstance(servers, dict) else servers if isinstance(servers, list) else []
            valid = server(value) or (bool(candidates) and all(server(v) for v in candidates))
            rule = "http_mcp_descriptor_heuristic_v1"
        return ("verified_json_descriptor" if valid else "unrecognized_json"), {"structural_rule": rule}
    if (len(text.strip()) >= 40 and
        re.search(r"^\s{0,3}#{1,6}\s+\S|\[[^\]\n]+\]\(https?://[^)]+\)", text, re.M) and
        not re.search(r"^\s*(?:#{1,6}\s*)?(404\b|not found\b|access denied\b|error\b)", text, re.I)):
        return "verified_markdown_text", {}
    return "unstructured_text", {}


def probe(base, path, spacing, timeout=8, max_bytes=65536, max_redirects=5):
    url, chain = base + path, []
    record = {"path": path, "requested_url": url, "redirect_chain": chain}
    start = time.monotonic()
    opener = urllib.request.build_opener(NoRedirect())
    try:
        for hop in range(max_redirects + 1):
            spacing.wait(url)
            req = urllib.request.Request(url, headers={
                "User-Agent": USER_AGENT, "Accept": "application/json, text/plain, text/markdown;q=0.9, */*;q=0.1",
                "Accept-Encoding": "identity"})
            then = time.monotonic()
            try:
                response = opener.open(req, timeout=timeout)
            except urllib.error.HTTPError as exc:
                response = exc
            with response:
                status = response.code
                location = response.headers.get("Location")
                chain.append({"url": url, "status": status, "location": location,
                              "latency_ms": round((time.monotonic()-then)*1000, 2)})
                if status in (301, 302, 303, 307, 308) and location:
                    new = urllib.parse.urljoin(url, location)
                    origin(new)  # reject unsupported redirect schemes and credentials
                    if urllib.parse.urlsplit(url).scheme == "https" and urllib.parse.urlsplit(new).scheme != "https":
                        raise ValueError("HTTPS downgrade refused")
                    if hop == max_redirects:
                        raise ValueError("Redirect limit exceeded")
                    url = new
                    continue
                data = bytearray()
                deadline = time.monotonic() + timeout
                while len(data) <= max_bytes:
                    if time.monotonic() > deadline:
                        raise TimeoutError("body deadline exceeded")
                    chunk = response.read1(min(8192, max_bytes + 1 - len(data)))
                    if not chunk:
                        break
                    data.extend(chunk)
                truncated = len(data) > max_bytes
                raw = bytes(data[:max_bytes])
                content_type = response.headers.get("Content-Type", "")
                charset = response.headers.get_content_charset() or "utf-8"
                try:
                    text = raw.decode(charset, errors="replace")
                except LookupError:
                    text = raw.decode("utf-8", errors="replace")
                label, evidence = classify(path, status, url, content_type, text, truncated, len(chain)>1)
                record.update(status=status, final_url=url, content_type=content_type,
                              body=text, body_base64=base64.b64encode(raw).decode(),
                              body_sha256=hashlib.sha256(raw).hexdigest(), bytes_read=len(raw),
                              truncated=truncated, classification=label, evidence=evidence,
                              verified=label in VERIFIED)
                break
    except Exception as exc:
        reason = exc.reason if isinstance(exc, urllib.error.URLError) else exc
        kind = ("timeout" if isinstance(reason, (TimeoutError, socket.timeout)) else
                "tls_error" if isinstance(reason, ssl.SSLError) else "network_error")
        record.update(classification=kind, verified=False, error_type=type(reason).__name__,
                      error=str(reason), final_url=url, status=None)
    record["latency_ms"] = round((time.monotonic()-start)*1000, 2)
    return record


def scan(targets, workers=6, spacing=0.3, timeout=8, max_bytes=65536, cohorts=None):
    if not 1 <= workers <= 32 or spacing < 0 or timeout <= 0 or not 128 <= max_bytes <= 1048576:
        raise ValueError("Invalid worker, spacing, timeout or byte bounds")
    bases = list(dict.fromkeys(origin(t.strip()) for t in targets))
    limiter = Spacing(spacing)
    def one(base):
        return {"origin": base, "cohort": (cohorts or {}).get(urllib.parse.urlsplit(base).hostname, "custom"),
                "probes": [probe(base, p, limiter, timeout, max_bytes) for p in PATHS]}
    with concurrent.futures.ThreadPoolExecutor(max_workers=workers) as pool:
        results = list(pool.map(one, bases))
    return {"schema": "agent_ready_radar.scan", "schema_version": 1, "version": VERSION,
            "observed_at": dt.datetime.now(dt.timezone.utc).isoformat(),
            "config": {"workers": workers, "spacing_seconds": spacing, "timeout_seconds": timeout,
                       "max_bytes": max_bytes, "user_agent": USER_AGENT},
            "domains": results}


def statistics(data):
    domains = data["domains"]
    by_path = {}
    for path in PATHS:
        probes = [p for d in domains for p in d["probes"] if p["path"] == path]
        naive = sum(p.get("status") == 200 for p in probes)
        verified = sum(p["verified"] for p in probes)
        false = sum(p.get("status") == 200 and not p["verified"] for p in probes)
        illusion = sum(p.get("status") == 200 and p["classification"] in
                      ("soft_404_html", "redirected_to_home", "login_wall", "parked_redirect") for p in probes)
        by_path[path] = {"verified": verified, "naive_200": naive, "false_positives": false,
                         "html_redirect_illusions": illusion}
    focus = [p for d in domains for p in d["probes"] if p["path"] in ILLUSION_PATHS]
    return {"domains": len(domains), "by_path": by_path,
            "any_verified_artifact": sum(any(p["verified"] for p in d["probes"] if p["path"] != "/robots.txt") for d in domains),
            "false_positive_domains": sum(any(p["path"] in ILLUSION_PATHS and p.get("status") == 200 and not p["verified"]
                                       for p in d["probes"]) for d in domains),
            "focus_naive": sum(p.get("status") == 200 for p in focus),
            "focus_html_redirect_illusions": sum(p.get("status") == 200 and p["classification"] in
                ("soft_404_html", "redirected_to_home", "login_wall", "parked_redirect") for p in focus),
            "focus_false": sum(p.get("status") == 200 and not p["verified"] for p in focus)}


def report(data):
    s = statistics(data)
    n = s["domains"]
    lines = ["# Empirical Web-Agent Surface Survey", "", "Observed: " + data["observed_at"],
             "", f"Sample: {n} purposively selected domains; not a random web-wide estimate.",
             "", "## Artifact adoption", "",
             "| Path | Verified / domains | Adoption | Naive final 200 | False positives | HTML/root/login/parked illusions |",
             "|---|---:|---:|---:|---:|---:|"]
    for p, v in s["by_path"].items():
        lines.append(f"| `{p}` | {v['verified']}/{n} | {100*v['verified']/n if n else 0:.1f}% | {v['naive_200']} | {v['false_positives']} | {v['html_redirect_illusions']} |")
    lines += ["", "## Soft-404 / redirect illusion", "",
              f"Across llms.txt, agents.md and both MCP paths: {s['focus_false']}/{s['focus_naive']} naive final-200 positives fail structural verification "
              f"({100*s['focus_false']/s['focus_naive'] if s['focus_naive'] else 0:.1f}%). "
              f"{s['false_positive_domains']}/{n} domains have at least one such false positive.",
              "False positives include unrecognized JSON/text, truncation and WAF bodies, not only HTML/redirects; see the separate illusion column.",
              f"The narrower HTML/root/login/parked illusion count is {s['focus_html_redirect_illusions']}/{s['focus_naive']} "
              f"({100*s['focus_html_redirect_illusions']/s['focus_naive'] if s['focus_naive'] else 0:.1f}% of naive positives).",
              "", "## AI crawler policy by cohort", "",
              "Counts are domain-bot pairs (10 bots per domain). Explicit named groups only; wildcard rules are not attributed as explicit AI policy. "
              "'Disallow' can be path-specific, not necessarily a site-wide ban. Mixed means both allow and non-empty disallow. "
              "Unknown robots responses are not treated as allow.", "",
              "| Cohort | Domains | Verified robots | Allow | Disallow | Mixed | Unspecified | Unknown |",
              "|---|---:|---:|---:|---:|---:|---:|---:|"]
    for cohort in sorted({d["cohort"] for d in data["domains"]}):
        ds = [d for d in data["domains"] if d["cohort"] == cohort]
        counts = dict.fromkeys(("allow", "disallow", "mixed", "unspecified", "unknown"), 0)
        good = 0
        for d in ds:
            p = next(p for p in d["probes"] if p["path"] == "/robots.txt")
            if p["classification"] == "verified_robots":
                good += 1
                for bot in p["evidence"]["bots"].values():
                    counts[bot["stance"]] += 1
            else:
                counts["unknown"] += len(BOTS)
        lines.append(f"| {cohort} | {len(ds)} | {good} | " + " | ".join(str(v) for v in counts.values()) + " |")
    lines += ["", "## Reproducibility and limits", "",
              "Single observation per path, GET, ordinary TLS validation, explicit redirects (max 5, no HTTPS downgrade), no browser/JS, no retries or API keys. "
              "One host task at a time; global hostname spacing also covers redirect destinations. Configuration is embedded in raw JSON.",
              "Bodies are capped; truncated artifacts are unverified, so large llms-full documents can be undercounted. "
              "Timeouts, TLS errors and blocking are unknown availability, not proof of absence.",
              "Markdown verification checks structure, not authorship, correctness or agent utility. MCP discovery paths have no universal descriptor schema; "
              "the versioned heuristic requires HTTP transport plus endpoint URL (or a map/list of such servers), and does not handshake with servers. "
              "Plugin checks cover v1 core fields, not full specification conformance. Redirect-to-home catches root redirects but not every possible parked site.",
              "Robots group extraction is descriptive, not an RFC-complete path-permission engine. This study does not establish MCP protocol support or universal agent readiness.",
              "", "## Per-domain observations", "",
              "| Cohort | Domain | " + " | ".join(PATHS) + " |",
              "|---|---|" + "---|" * len(PATHS)]
    for d in data["domains"]:
        lines.append("| " + d["cohort"] + " | " + d["origin"] + " | " +
                     " | ".join(p["classification"] for p in d["probes"]) + " |")
    return "\n".join(lines) + "\n"


def card(data):
    s = statistics(data)
    return {"schema": "octopus.client_surface", "schema_version": 1, "release_channel": "stable",
            "id": "home.agent-ready-radar", "revision": 1, "title": "Agent-ready radar", "glyph": "sites",
            "target": {"kind": "voice.widget", "slot": "right", "order": 20,
                       "layout": {"min_width": 184, "preferred_width": 300, "max_width": 460,
                                  "min_height": 100, "preferred_height": 200, "max_height": 360, "content_fit": "reflow"}},
            "data": {"observed_at": data["observed_at"], "statistics": s},
            "render": {"kind": "native", "density": "compact", "root": {
                "type": "stack", "align": "stretch", "gap": "sm", "children": [
                    {"type": "metric", "label": "Domains with verified artifacts", "value": f"{s['any_verified_artifact']} / {s['domains']}"},
                    {"type": "text", "role": "body", "value": f"{s['focus_false']} of {s['focus_naive']} naive positives fail verification."},
                    {"type": "text", "role": "caption", "tone": "muted", "value": "Snapshot: " + data["observed_at"] + " · heuristic checks, not protocol conformance."}
                ]}}}
