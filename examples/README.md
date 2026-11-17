# Standalone Examples & Empirical Benchmarks

Two MIT-licensed companion packages illustrate narrow, testable pieces of
agent infrastructure. They run without installing Octopus, signing in to a model,
or configuring its runtime. They are **not** kernel components, production
security certifications, or evidence of autonomous self-improvement.

## [mcp-identity-guard 0.2.0](mcp-identity-guard/README.md)

A Unix Rust library and CLI for a provider-qualified MCP stdio gateway:
reviewed executable SHA-256/metadata pins, scoped secret handles, bounded
multi-encoding scrubbing, finite-graph minimum-cost conjunctive recourse, and a
hash-chained audit ledger. Commands: `lock`, `proxy`, `verify-ledger`, `bench`,
`oracle-bench`.

Three-line quickstart from the repository root (stable Rust, Unix):

```sh
cd examples/mcp-identity-guard
cargo test --locked
cargo run --release --locked -- oracle-bench
```

The [recorded benchmark](mcp-identity-guard/BENCHMARK_REPORT.md) measured **2.418 ms
p50 paired proxy overhead** and **0/5 spoof false-accepts** for a trusted
process oracle versus **5/5** for a deliberately optimistic substring verifier.
There was only **one benign accuracy trial**; timing repetitions do not add
accuracy trials. These are synthetic local fixtures, not a population estimate,
a replication of a training study, or proof of general attack resistance.

The guard is not a sandbox. Open-descriptor verification still precedes
pathname-based spawn; scrub coverage is bounded; hash chains need a trusted
anchor; recourse minimality holds only within the declared finite repair graph.
See the package's [threat model](mcp-identity-guard/README.md#audit-and-threat-model).

A separate [local native-room smoke test](mcp-identity-guard/README.md#local-native-octopus-integration-smoke-test)
also listed and called a qualified Python diagnostics tool through Octopus's
Seatbelt-enforced MCP stdio room, closed cleanly, and produced a verified
7-record audit chain. This demonstrates one configured integration, not general
security or stock-installation support.

## [agent-ready-radar 0.1.0](agent-ready-radar/README.md)

A zero-dependency Python 3.12+ CLI and library for bounded HTTP observations of
`/llms.txt`, `/llms-full.txt`, `/agents.md`, two MCP discovery paths,
plugin manifests, and explicit robots AI directives. Commands: `scan`, `report`,
`octopus-card`, plus `summarize` for body-free publication.

Three-line quickstart from the repository root (offline report; Python 3.12+):

```sh
cd examples/agent-ready-radar
export PYTHONPATH=src
python3 -m agent_ready_radar report benchmarks/survey-2026-11-17-summary.json
```

The [November 17, 2026 survey](agent-ready-radar/SURVEY_2026_11_17.md) covered
**60 purposively selected domains / 420 probes**: 33/60 structurally verified
`/llms.txt` and 9/60 `/agents.md`. Across `/llms.txt`, `/agents.md`, and
both MCP paths, **20/66 (30.3%)** naive final-HTTP-200 positives failed verification.
That denominator is not all 420 probes; false positives include truncation and
unrecognized bodies, not just soft-404 HTML.

The [227 KB summary](agent-ready-radar/benchmarks/survey-2026-11-17-summary.json)
retains all probe metadata and full SHA-256 hashes, redirect chains, robots AI
rules, and aggregate counts, without raw response bodies. `scan` reproduces full
raw captures on demand; the date's responses are not promised to recur.
This is one dated, nonrandom snapshot: structural verification does not establish
authorship, semantic usefulness, or a working MCP handshake. Blocking and
network failures are unknown availability, not absence. Only scan authorized hosts.

## Provenance and verification

Exported from the standalone source snapshots `mcp-identity-guard@7b5543a` and
`agent-ready-radar@aaaf31a`. The gateway code and measured JSON are unchanged;
machine-specific documentation/log paths were removed. The radar export adds
portable root-level test invocation and deterministic body-free summary
encoding/reading. The original scanner and classification rules are unchanged.

Each package carries its own license. Third-party observed response content
is not relicensed by the code's MIT license. The Rust example is excluded from
the root Cargo workspace and has its own lockfile.

From the repository root:

```sh
python3 -m unittest discover -s examples/agent-ready-radar/tests -p 'test_*.py'
cargo test --manifest-path examples/mcp-identity-guard/Cargo.toml --locked
cargo clippy --manifest-path examples/mcp-identity-guard/Cargo.toml --locked --all-targets -- -D warnings
```

The Ubuntu `client` and `rust` jobs in [quality](../.github/workflows/quality.yml)
run the corresponding package tests. These examples do not change the Octopus
release version, installation, or live runtime.
