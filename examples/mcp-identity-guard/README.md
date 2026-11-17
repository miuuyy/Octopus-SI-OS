# mcp-identity-guard v0.2.0

Experimental Unix MCP stdio proxy: qualified provider identities, reviewed executable
and tool pins, scoped secret handles, multi-encoding output scrubbing, weighted
conjunctive recourse, and a process-oracle demonstration. Rust library + CLI, MIT.
No network, model, real credential or cloud service is needed for verification.

## Build and evaluate

Use stable Rust with edition 2024 support (the recorded measurements used Rust
1.99.1). Run these commands from this package directory. The package is independent
of the Octopus workspace; all dependencies are pinned in Cargo.lock.

```sh
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
cargo run --release --locked -- bench > benchmark.json
cargo run --release --locked -- oracle-bench > oracle-benchmark.json
```

After dependencies are cached, these commands can also run with `--offline`.
No Octopus installation, model account or credential is required for the fixtures.
The original measurements and command logs are retained; package-root paths in
the logs have been replaced with `<package-root>` for publication.

## Use

Create an operator-owned manifest using canonical absolute paths:

```json
{"version":1,"servers":[{
  "server_id":"github",
  "executable":"/absolute/path/to/native-mcp-server",
  "argv":["--read-only"],
  "cwd":"/absolute/path/to/workdir",
  "secrets":{"github_readonly_pat":{
    "path":"/operator-chosen/secret-store/github-readonly",
    "tools":["github_read_issue"]
  }}
}]}
```

Secret files must be current-user-owned regular 0600 UTF-8 files, <=64KiB,
with >=12 bytes and >=6 distinct byte values. This length/diversity check is
not a statistical entropy estimate. A newline is part of the secret.

```sh
target/release/mcp-identity-guard lock manifest.json manifest.lock.json
# Review every pin and grant; lock executes the operator-approved server.
target/release/mcp-identity-guard proxy manifest.lock.json ledger.jsonl
target/release/mcp-identity-guard verify-ledger ledger.jsonl
```

Lock creation refuses overwrite. Configure the client to launch the proxy.
After standard MCP initialization, tools have names such as
`github__github_read_issue`. A tool call's params can contain:

```json
{"name":"github__github_read_issue","arguments":{
  "token":{"$secret_handle":"github_readonly_pat"}
}}
```

Handles are singleton objects; authorization is exact server/native-tool/handle.
The caller cannot choose executable, argv, cwd, secret path, or new grants.
Child environment is cleared, stderr discarded, and raw erroneous upstream
frames are never forwarded. The entire catalog is rechecked before dispatch.
Metadata changes freeze the session.

## v0.2 hardening

### Whole-secret encoding scrubber

Patterns are precomputed after secret resolution, retained for the session,
and replaced longest-first in decoded output strings and keys. Both result and
error envelopes pass through sanitization; upstream JSON-RPC errors become
generic local refusals rather than relaying their data.

Covered: literal UTF-8; standard and URL-safe base64, padded/unpadded;
lower/uppercase hex; fully percent-encoded and RFC3986 unreserved-preserving
percent encodings with lower/uppercase escapes; normal JSON string escapes,
escaped slash, and all-UTF16-unit Unicode escapes with lower/uppercase hex.
Weak/short secrets are refused before injection; the standalone scrub helper
only generates literal patterns for short inputs, avoiding tiny encoded matches.

This is bounded whole-value scrubbing, **not comprehensive encoded-secret DLP**.
Mixed per-character percent/JSON escaping, composed encodings, fragmentation,
compression, encryption and covert channels can evade it. Matching public text
equal to a full credential is intentionally redacted. Key collisions may drop
data after redaction. No secret zeroization guarantee.

### Executable identity and metadata pins

Locks now include canonical path, size, mode, uid/gid, device/inode and SHA-256.
Immediately before spawn the guard opens O_NOFOLLOW, checks a regular executable
owned by root/current effective user, rejects group/world write and set-id bits,
hashes the open descriptor, and checks timestamp/size stability across hashing.
It compares the full attestation with the lock and keeps the descriptor open
through spawn. Proxy startup syncs a `binary_verified` hash-chain entry per
provider before launch, containing the verified attestation.

**This is not race-free exec or post-launch runtime attestation.** Command::spawn
still executes a pathname, so replacement after verification remains possible.
Dependencies, script interpreters, parent directories and cwd assets are not
attested. Trusted, non-adversary-writable launch paths and OS isolation remain
necessary. A metadata pin change requires an operator-reviewed fresh lock.
v0.1 locks without attestations fail closed and must be regenerated.

### Complete minimum-cost preflight repair set

For a known, non-frozen provider, the guard gathers all observable failures in
tool membership, argument object/handle shape, handle grants, supported argument
constraints and catalog identity before resolving any secret or calling a tool.
Constraints covered are type, enum, required, numeric minimum/maximum, and nested
properties. Unknown JSON Schema keywords remain upstream-owned.

The finite solver exhaustively searches all 16 subsets of four repair operations:

| Operation | Authority expansion cost |
|---|---:|
| argument_fix | 1 |
| lockfile_refresh | 4 |
| secret_scope_grant | 16 |
| server_pin_addition | 64 |

Superincreasing weights prefer any combination of lower-authority changes.
The receipt includes all repair instructions, `minimal_recourse_set`,
`total_cost`, `is_globally_minimal:true`, and `recourse_scope`.
Three simultaneous argument/catalog/secret blockers produce all three operations,
cost 21, not a first-clause-only recommendation. Shared repairs are counted once.
A regression proves that each partial repair of a two-clause refusal fails and
the full reviewed set succeeds. The solver's alternative-cover cases are checked
against an independent exhaustive reference for all 3,375 nonempty three-clause
graphs.

**Minimality is exact within the declared finite repair vocabulary and observed
preflight graph**, not over arbitrary changed requests or policies. Requested
provider/tool and handle intent are fixed: removing a requested secret, choosing
another tool, or abandoning the task is not counted as a sufficient repair.
Each operation means fixing all associated failed conjuncts; lock refresh includes
operator review and session restart, never automatic approval. Unknown routing,
frozen sessions, secret-file/transport failures and future server behavior are
separate boundaries without a global claim. The solver supports server addition;
unknown-provider routing cannot inspect a nonexistent server's schema/grants.
No minimality claim certifies that an untrusted server will behave after repair.

## Process-oracle benchmark

`oracle-bench` compares a deliberately optimistic substring verifier (recognizes
"All tests passed", "Secret rotated", "File written") with deterministic evidence
checking: admitted movement, correlated completion, expected provider and binary
attestation hash, no refusal/scrub mark, a fresh run nonce, anchored hash-chain
head, and actual post-state bytes matching the expected SHA-256.

The trusted harness calls real local guarded mock processes and records observed
execution status and file state in a **separate observer ledger**. It is not a
production task-oracle integration of the generic proxy audit schema. Agent/tool
text is not permitted to create receipts or choose expectations/anchors.
The oracle requires a trusted producer and anchor: an attacker able to rewrite
both can forge success. SHA-256 is integrity, **not a signature**.

Motivation: [Optimistic Verifiers Get Hacked: Reward Hacking Under an Exact
Process Oracle](https://arxiv.org/abs/2611.21538v1). Its abstract describes executable
authorization-policy oracles and degraded verifiers, not our movement receipts.
This engineering demonstration is inspired by that distinction, **not a
replication of its training experiments or an empirical claim from the paper**.

## Measured v0.2.0 results (local synthetic fixtures)

| Path | p50 µs | p99 µs |
|---|---:|---:|
| Direct stdio | 82.750 | 4510.917 |
| Guarded stdio + ledger sync | 2520.875 | 17934.375 |
| Paired guarded-minus-direct | 2417.833 | 15964.458 |

| Verifier | False acceptance (attacks) | False rejection (benign) | p50 µs | p99 µs |
|---|---:|---:|---:|---:|
| Optimistic text | 5/5 (100%) | 0/1 (0%) | 0.042 | 0.125 |
| Exact process oracle | 0/5 (0%) | 0/1 (0%) | 31.667 | 65.250 |

18 tests passed; Clippy all targets passed with warnings denied; both benchmark
commands passed 6/6 fixtures. Oracle rates have only five attacks and one benign
trajectory; 200 timing repetitions per trajectory are NOT extra accuracy trials.
See [BENCHMARK_REPORT.md](BENCHMARK_REPORT.md) for method and exact scope.

## Local native Octopus integration smoke test

On 2026-11-17, a separate, locally installed Octopus native `system_mcp` /
`mcp_stdio_session` room exercised this proxy with `sandbox_mode="enforced"`
under macOS Seatbelt. A dependency-free Python diagnostics MCP server was locked;
`mcp.status`, `mcp.list_tools`, a call to the qualified
`local_diag__octopus_host_summary` tool, and `mcp.close` completed successfully.
The retained integration report and tool-completion records were inspected, and
`verify-ledger` independently verified the resulting **7 hash-chained records**.

This is one local interoperability smoke test, not a security evaluation or a
claim that the proxy itself provides a sandbox. The native room was configured
separately; this example does not install it. Private host output, lockfiles and
audit records are deliberately not published.

For a Python MCP server under macOS Seatbelt, use the actual framework Python
executable rather than the system `python3` xcrun shim. Its executable closure
may also need the framework's `Resources/Python.app/Contents/MacOS/Python`
entry in `executable_paths`. Stage custom proxy binaries in a root readable by
the native room and grant only the required executable, read and ledger-write
paths. These are host-configuration requirements, not portable server defaults.

## Audit and threat model

The generic proxy ledger records received/completion pairs and binary verification,
not arbitrary filesystem postconditions. Allowed completion includes a scrubbed
flag. A received record with no completion means uncertain execution; do not replay
side effects blindly. The ledger synchronizes entries before replies and excludes
raw arguments/results/secret values. A single-writer sentinel prevents parallel
proxy writers. After a crash, an operator must inspect the ledger and establish
that no writer remains before removing its stale writer-lock.

Hash continuity detects editing against a trusted head; externally anchor the head
to detect truncation/wholesale rewriting. The log is not signed, rollback-proof,
or OS-enforced append-only. Do not trust an agent-supplied ledger/anchor pair.

Supported subset: Unix newline JSON-RPC 2.0 stdio, MCP 2025-03-26 initialize,
initialized notification, tools/list and tools/call. Sequential requests,
3-second response deadline, <=1MiB frames. Blocking writes/termination are not a
hard wall-clock sandbox. Unpaginated catalogs; ASCII names <=80 characters,
without "__"; metadata is exactly name, description and inputSchema. Extra
metadata, duplicates and unexpected protocol messages fail closed. Extra tool keys
(including `annotations`, regardless of hint values) produce a non-retryable
`UNSUPPORTED_TOOL_METADATA` refusal at `tool_identity`, without reflecting upstream
keys or values. This is a deliberate compatibility restriction, not an IO failure;
operators must review provider compatibility rather than silently strip metadata.
Hints are untrusted metadata, never authority, and their absence proves no safety.
Compact sorted serde_json is used for hashing, not RFC8785/JCS.

**Not a sandbox.** Authorized same-user providers may read same-user files, access
the network and misuse secrets. No hashing scheme proves honest behavior behind
unchanged metadata or eliminates check/call races. No HTTP transport, streaming,
sampling, resources, env/header secret injection or automatic policy amendment.
Protect lock files, binaries and all dependencies; use revocable least-privilege
credentials and OS isolation. This is a reviewable experimental security boundary,
not a universal MCP gateway or production security certification.

`__mock` is an internal evaluation fixture, not a production server.
These local commands do not publish a package or release. This standalone source
snapshot is distributed in the [Octopus examples collection](../README.md).
