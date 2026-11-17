# v0.2.0 benchmark report — 2026-11-17

## Environment and verification

Rust 1.99.1, Darwin 25.6.0, arm64. Offline, locked dependencies; no new crates.
18 tests passed, 0 failed. Clippy all targets passed with warnings denied.
Both release benchmark commands exited successfully, no compiler warnings.
Raw command output: test-results.txt, clippy-results.txt, benchmark-build.txt,
oracle-build.txt. Raw measurements: benchmark.json and oracle-benchmark.json.

## Existing MCP stdio benchmark: 6/6

Real Rust child processes; three providers share one native tool name. Synthetic
credential only. Fixtures: benign scoped injection, flat-name shadowing refusal
and explicit provider preservation, catalog rug-pull refusal/session freeze,
secret echo scrubbing, cross-provider secret denial, successful argument recourse.
Attack mitigation 4/4 fixtures, benign false positives 0/200 measured calls.
These are engineering smoke checks, not general attack-resistance estimates.

20 warmup pairs, 200 measured sequential direct/guarded pairs, nearest-rank p50/p99.
Guarded time includes stdio, catalog recheck, secret read/injection, cached
multi-encoding scrub patterns, JSON processing, and two synchronized ledger writes.
Startup hashing and its three synchronized binary-verification records are outside
call timing. Direct calls supply the same synthetic token directly.

| Path | p50 µs | p99 µs |
|---|---:|---:|
| Direct stdio | 82.750 | 4510.917 |
| Guarded stdio + ledger sync | 2520.875 | 17934.375 |
| Paired guarded-minus-direct | 2417.833 | 15964.458 |

The overhead p50 is 2.418 ms.
Paired difference percentiles are not differences of independent percentiles.
Direct always runs first; scheduling, filesystem sync and simultaneous host work
can bias results. No confidence interval or cross-machine claim is made.

Ledger verification: **445 records** = 3 binary verifications +
2 × (1 initialize + 20 warmups + 200 calls).
Observed head: `6342a5beeef71f853ec1f09b24f4efb78521ed9304dd5c60f2d591cc13da70f6`.
Fixture files are removed after verification; this is not a persisted audit stream.

## Process-oracle benchmark: 6/6 expected classifications

| Verifier | False acceptance (attacks) | False rejection (benign) | p50 µs | p99 µs |
|---|---:|---:|---:|---:|
| Optimistic text | 5/5 (100%) | 0/1 (0%) | 0.042 | 0.125 |
| Exact process oracle | 0/5 (0%) | 0/1 (0%) | 31.667 | 65.250 |

| Trajectory | Ground truth | Optimistic | Exact |
|---|---|---|---|
| Benign guarded write | Success | Accept | Accept |
| Success claim after silent failure/no write | Failure | Accept | Reject |
| Uncalled-tool fabrication | Failure | Accept | Reject |
| Wrong provider, identical response shape | Failure | Accept | Reject |
| Scrubbed secret exfiltration masked as success | Failure | Accept | Reject |
| Replayed stale nonce/receipt | Failure | Accept | Reject |

Accuracy denominators: five attack trajectories, one benign trajectory.
False acceptance = accepted attacks / all attacks; false rejection = rejected
benign / all benign. No division by timing repetitions. The optimistic verifier
uses explicit success substrings, not an installed regex dependency.

Each case uses a fresh file/ledger location. A trusted observer calls the real
guarded mock (except the uncalled-tool case), measures post-state, records movement
admission and completion, and supplies an independently retained expected provider,
binary-attestation hash, post-content digest and current nonce. Wrong provider
truly calls B rather than A. The scrubbed attack truly injects/echoes a synthetic
credential and checks the guard's scrub mark. The stale trajectory records an old
nonce although post-state is correct. Silent failure omits the state write and
records a failed outcome.

Exact verification reads the ledger, checks full sequence/hash continuity and
trusted anchored head, requires exactly the admitted/completed pair, matches
nonce/provider/attestation, checks allowed outcome and scrubbed=false, then reads
and hashes actual post-state. Extra refusal entries cannot be ignored.
Negative controls change the anchor, expected attestation and file content and
require rejection; independent tests also edit ledger events and require failure.

Latency uses 200 repetitions per case (1,200 observations per verifier), with
black_box to inhibit elimination. Warm-cache filesystem reads and hashes are
included for exact; execution/receipt production are excluded. Raw microseconds
are retained. A failed case may short-circuit before all checks, so aggregate
latency mixes paths and is not equivalent to a full successful verification cost.

## Hardening regression evidence

- Literal + standard/URL-safe base64 padded/unpadded, hex case variants,
  percent encodings and JSON escapes in result/error strings and keys.
  Independent known encoding vectors supplement generated variants.
- Real upstream encoded echoes scrubbed; encoded upstream JSON-RPC error data
  suppressed; weak secrets refused before injection.
- Group/world-writable executables refused; canonical symlink aliases refused;
  same-content permission changes break full metadata pins.
- Proxy ledger contains the verified binary metadata/digest for all providers.
- Simultaneous argument + grant failure: each partial repair still fails; complete
  reviewed set succeeds. Argument + grant + catalog failure returns all three,
  total cost 21. Independent exhaustive reference checks 3,375 solver graphs.

## Scope boundaries

See README. Pre-spawn descriptor metadata/digest pinning is **not race-free
path-based exec**, and does not attest shared libraries, interpreter or behavior.
Whole-secret encodings are covered, not mixed/composed/fragmented/encrypted
exfiltration. Length/diversity is not measured entropy. Global repair minimality
is only over the fixed finite vocabulary and observed preflight clauses, preserving
requested tool/handle intent; not arbitrary future behavior or full JSON Schema.

The process-oracle benchmark uses trusted-harness observer receipts, not a
production filesystem oracle built into the generic proxy ledger. Hash-chain
integrity requires trusted expectations/anchor and does not authenticate an
attacker-controlled ledger. No signatures, remote attestation or sandbox.

[arXiv:2611.21538v1](https://arxiv.org/abs/2611.21538v1) motivates the verifier-gap
distinction using executable policy oracles. Our five local spoofing scenarios,
movement schema and timings are our own; this is not that paper's reproduction
or a model training benchmark. This source snapshot is distributed in the
[Octopus examples collection](../README.md), not as a separate package release.

## Reproduce

```sh
cargo test --offline --locked
cargo clippy --offline --all-targets -- -D warnings
cargo run --offline --release --locked -- bench > benchmark.json
cargo run --offline --release --locked -- oracle-bench > oracle-benchmark.json
```

See [README.md](README.md) for portable build instructions. Recorded logs retain
the original results, with package-root paths replaced by `<package-root>`.
