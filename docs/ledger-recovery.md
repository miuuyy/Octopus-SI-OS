# Ledger recovery scaling

The movement ledger remains authoritative. SQLite is a disposable lookup cache,
not permission evidence.

- Client doctor scans the **whole** ledger as UTF-8 rows, retaining only evidence
  for currently running runs. Memory is proportional to the largest row and the
  running-run set, not total ledger size. Malformed trailing rows still fail;
  empty lines, line numbers, final unterminated rows, and unavailable-ledger
  diagnostics retain their existing semantics.
- Provider source replay uses a SQLite byte-range hint for the requested sequence.
  It verifies row boundaries, sequence, outbound direction, target interface,
  runtime interface/run, and the existing bounded artifact/binding envelope.
  Missing, behind, or mismatched hints fall back to sequential replay. Corrupt
  requested source/envelope evidence is not repaired or silently accepted.
- The provider evidence index shares exactly equal immutable readable-artifact
  sets between historical sources via a one-entry exact `artifact_access` cache
  and exact set interning. It never unions different grants. Spotlight's
  pre-validation pass is retained: a stale candidate set must not leave a
  partially mutated recovery index.

## Why completed capability amendments are not deleted

`capability_amendment` is checked transition evidence, not a disposable execution
cache. Its captured manifest/payload authenticates the bridge call and the exact
future-contract delta. Checked provider history reconstructs the ordinary patch,
applies that delta, restores the proof, and compares the resulting bytes.
Deleting the proof is explicitly rejected as removal of capability transition
evidence; dropping its payload prevents effect verification.

Completion alone does not establish a safe collection frontier for older checked
run snapshots. New amendments keep payloads up to 64 KiB inline; larger
`effect.payload` values are version-1 references with an exact BLAKE3 digest and
byte count. The provider first writes the payload under its declared private
`capability-proofs/v1/<digest>` directory, syncs the file and directories, and
reads it back with digest/length verification before publishing the checked
record. Unix proof files are created with mode `0600`. Existing corrupt or
partial proofs are refused rather than overwritten. These files are retained
evidence, not an execution cache.

Checked replay captures referenced bytes through its evidence reader. Before
effect admission, the provider's exact matching owner context supplies the private
root. After admission, generic/foreign checked-file readers use the payload in
their already-declared blob handle. Neither route derives a read path from the
record's `manifest_path`, another interface's identity, or a run-file location.
Missing proof access fails closed. An existing malformed, truncated, symlinked or
wrong-digest blob fails rather than falling back to a good private copy. Private
proof paths reject symlinked subdirectories and carriers, and reads obey the
reader's payload bound. Reference version, digest and length are validated before
resolution. Resolution does not confer authority or replace bridge, manifest,
pending-effect, ordinary-patch, topology and exact after-byte checks.

Legacy inline payloads retain their exact serialized representation and replay
without proof-store access. No installed records are migrated or compacted. The
ordinary patch and reference-form after bytes are replayed exactly, not rewritten
into an inline approximation.

## Operational run tails and observation

`list_runs` requests a bounded descending SQLite tail per run and returns it in
ascending sequence order. Authoritative row decoding and sequence/run checks
remain; missing or unusable index hints still fall back to scanning the ledger,
including any unindexed tail.

`list_runs` allocates its output byte budget to directly connected peers before
other discovered runs, retaining activity priority within each group. Separate
writer runs are identified structurally from `file_writer_target` and their exact
active call/result checkpoint pair (never by run ID prefix): their declared
call/result routes are standing waits, not outstanding tool calls. Healthy idle
writers report `idle` even with no tail movements and sort into Tier 4 below
ordinary discovered runs (regardless of external inputs), while admitted calls,
`running` status, errors and refusals remain visible at their active/error tiers.
Returning a writer's result to its owner does not open a provider turn in the
writer run; malformed or incomplete writer declarations do not receive this
diagnostic exception.

Queued `interface.context.updated` notifications do not open a provider turn. On
an `idle` run record whose latest observed artifact is
`interface.context.updated`, a stale tail-derived open flag is cleared while
actual pending tool calls and `running` records retain their open phases.

## Read-only storage inspection

```sh
octopus storage
octopus storage --home /path/to/home --json
```

This offline command does not start or contact the daemon, bind a socket, create
a Home, repair files or delete evidence. Long Home/`TMPDIR` paths are supported.
OS aliases in ancestor paths are accepted; a symlink selected as the Home itself
is refused, and symlink entries within the selected Home are not followed. It
reports ledger/index/WAL/SHM/blob/metadata totals, provider-blind interface
directory totals (`interfaces.entries`: `{ id, total, top_directories }`), and
the five largest run files with their inline `capability_amendment` sizes
without reading blob payloads or probing provider-private paths.

JSON schema version 1 encodes byte quantities as exact decimal strings. Logical
bytes come from `lstat.size`; POSIX allocated bytes are `st_blocks * 512`. Counts
are per path, including directory and symlink entries; hard-link aliases count
separately, and allocation is not exclusive physical storage on cloned/compressed
filesystems. The live scan is non-atomic; read errors, changing run files,
malformed JSON and oversized run JSON produce explicit partial errors
(`ok:false`, exit 1), not invented success. Clean or absent Homes return exit 0.

## Native context message identity

Injected native context uses deterministic `msg_` IDs: URL-safe unpadded base64
of a domain-separated SHA-256 digest of the version tag, source sequence, and
context name. IDs are 47 ASCII characters regardless of input name length. The
context cache fingerprint includes the ID scheme version, so an old cached
fingerprint cannot bypass recovery validation after an upgrade. Recovery still
requires exact, complete, unique context messages for the exact thread.
Prior-turn v1 `msg_` context messages on the same thread are tolerated before
the current turn's items appear, while legacy `octopus-context-` IDs anywhere
in the rollout and malformed or in-turn altered context message IDs are refused
with a fresh-thread diagnostic rather than silently reinjected or accepted;
this does not mutate or repair any existing thread.
