# agent-ready-radar 0.1.0

A zero-dependency Python 3.12+ CLI for empirical observations of web-agent discovery
artifacts. HTTP 200 is **not** evidence of an agent interface: SPA catch-alls,
redirects, login pages and JSON error objects are rejected.

This standalone source snapshot is part of the [Octopus examples collection](../README.md).
Python 3.12+ is the installation target; source-mode contracts are also checked
with Python 3.9 for the older system interpreter. No third-party Python package
is needed to run from source or build the wheel/sdist.

## Run

```sh
python3 -m venv .venv
.venv/bin/python -m pip install --no-deps .
.venv/bin/python -m agent_ready_radar scan openai.com https://example.org --workers 4 -o scan.json
.venv/bin/python -m agent_ready_radar report scan.json -o report.md
.venv/bin/python -m agent_ready_radar octopus-card scan.json -o card.json
```

From source without installation: `PYTHONPATH=src python3 -m agent_ready_radar ...`.
The wheel and sdist builder uses only the standard library; no build dependencies
or runtime API keys. Console entry point: `agent-ready-radar`.

## Public survey data and reproduction

The committed [survey-2026-11-17-summary.json](benchmarks/survey-2026-11-17-summary.json)
is a deterministic, body-free **227,419-byte** record of all **60 domains / 420 probes**.
It preserves every domain/cohort, requested/final/redirect URL, every redirect
status and location, content type, byte length, **full SHA-256** (not just a
prefix), truncation, latency, classification verdict, verification evidence,
robots AI rules, scanner configuration, observation time, and aggregate statistics.
The roughly 25 MB original raw body capture is deliberately **not** distributed.

Schema `agent_ready_radar.summary` version 1 uses `probe_columns` and
`redirect_columns` to name the values in each ordered row. No compression or
external decoder is needed: zip a column list with its row. Optional absent
metadata in future scans is represented by null. `expand_summary` in
[`summary.py`](src/agent_ready_radar/summary.py) reconstructs body-free scan
records and checks aggregate counts. Both `report` and `octopus-card` accept
the summary directly:

```sh
PYTHONPATH=src python3 -m agent_ready_radar report benchmarks/survey-2026-11-17-summary.json
PYTHONPATH=src python3 -m agent_ready_radar octopus-card benchmarks/survey-2026-11-17-summary.json
```

To reproduce the **procedure and full raw captures** on demand, use
`python3 -m agent_ready_radar scan` with the original cohorts and bounds:

```sh
PYTHONPATH=src python3 -m unittest discover -s tests -v
PYTHONPATH=src python3 -m agent_ready_radar scan --targets-file benchmarks/cohorts.json --workers 6 --spacing .3 --timeout 8 -o benchmarks/survey-2026-11-17.json
PYTHONPATH=src python3 -m agent_ready_radar summarize benchmarks/survey-2026-11-17.json -o new-summary.json
PYTHONPATH=src python3 -m agent_ready_radar report new-summary.json -o new-report.md
```

Run from this package directory, and only contact authorized targets.
New responses, timestamps and counts will differ; rescanning cannot recreate
the historical bytes. The raw output filename above is gitignored.

This is a **single dated snapshot**, not a continuously refreshing Home feed.
The card can be published through an authorized Octopus Home presentation route;
writing it to disk does not display it.

## Measurement contract

Paths: `/llms.txt`, `/llms-full.txt`, `/agents.md`,
`/.well-known/mcp.json`, `/.well-known/mcp`,
`/.well-known/ai-plugin.json`, `/robots.txt`.

- Bounded cross-host concurrency; sequential paths per target; global per-host
  request spacing including redirect destinations. Default 64 KiB cap, 8-second
  socket/body deadline, five redirects, no retries. HTTPS downgrades refused.
- Raw JSON retains request/final URLs, every status/location/latency, text and
  base64 body prefix, SHA-256, truncation, errors and verification evidence.
- HTML content types/wrappers produce `soft_404_html` (canonical name; also known as
  html_soft_404). Root redirects produce `redirected_to_home`; 404 is `http_404`.
  Blocked responses, malformed JSON, unstructured text and truncation are separate.
- MCP JSON **heuristic**, not a universal schema: direct transport + HTTP(S)
  URL/endpoint, or nonempty `servers` / `mcpServers` maps/lists of these. Accepted
  transports: http, streamable-http, sse. Stdio local configs are not web endpoints.
  A valid descriptor is not proof that its server works; no MCP handshake occurs.
- Plugin manifests need v1 core names/descriptions, auth.type and OpenAPI URL.
- Markdown/text needs >=40 characters and a heading or HTTP Markdown link;
  HTML wrappers are rejected even when labeled text/plain. This does not prove
  provenance or semantic utility. Truncated files are conservatively unverified.
- Robots extraction preserves explicit named bot Allow/Disallow paths, reports
  mixed rules and unspecified policy. It is not an RFC-complete path evaluator.
  Wildcard policy is not counted as an explicit named AI crawler stance.
- Naive means **final** status 200 after redirects. Reports separate all rejected
  naive positives from the narrower HTML/redirect illusion count. Unknown network
  availability is not absence. Rates use all sampled domains as denominator.

Default User-Agent:
`agent-ready-radar/0.1.0 (+https://github.com/octopus-sios/octopus-os; empirical readiness research)`.

Only scan hosts you are authorized to contact. Do not expose this CLI as an
unrestricted server: user-supplied targets/redirects can reach local networks.
No robots impersonation, JS execution, authentication or WAF evasion.
Follow applicable site policies. A polite small research probe is not permission
to crawl or republish a site's content. Raw body prefixes are retained as audit
evidence, not licensed for reuse.

## License
MIT (code). Observed third-party response content remains its owners' material.
