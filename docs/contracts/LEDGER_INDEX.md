# The ledger and its indexes

The system's core record is not a message bus or an event store. It is the
**log of artifacts and their movements**. Nothing in the framework works
without artifacts, and every meaningful action is an artifact crossing an
interface boundary, so this from→to ledger is what makes the whole system
traceable and replayable.

## The movement log

`ledger/movements.jsonl` is the source of truth: append-only, flushed per row,
replayable. Every row is one movement between two exact interface folders.

The following shows selected fields; content hashes and metadata are abbreviated:

```json
{
  "seq": 0,
  "lamport": 1,
  "recorded_at_unix_ms": 1791528512739,
  "trace": "trace:run_home:cp_operator_input",
  "kind": "InterfaceArrival",
  "from": { "Interface": "world_operator_chat_home" },
  "to":   { "Interface": "system_worker_operator_home" },
  "artifact": { "id": "operator.input:ac16…", "content": [172, 22, 24] },
  "invocation_metadata": { "runtime": { "run": "run_home", "checkpoint": "cp_operator_input", "epoch": 0, "ee": "ee_existence" } }
}
```

Endpoints are always interfaces. The run, checkpoint, EE, worker, grant, mark
outcome, and binding snapshot are typed provenance metadata *on* the edge,
never alternate endpoint kinds.

In durable invocation rows, `invocation_metadata.grant_content` references the
stored grant projection, and non-empty binding reference lists use
`artifact_binding_refs_content`. Both are canonical BLAKE3 addresses in
`ledger/metadata/`, so repeated content is stored once. Decoding verifies and
restores the typed fields; providing both inline and addressed forms of a field
is invalid. Metadata content belongs to the durable truth and cannot be deleted
as an index cache. Readers that reuse a checked prefix revalidate the metadata
files they used. Formation still recomputes authority at the next arrival. See the
[artifact and metadata example](ARTIFACTS.md#payload-manifest-and-delivery-metadata).

| Kind | Meaning |
| --- | --- |
| `InterfaceArrival` | An artifact was admitted at a checkpoint pinning the arriving interface. |
| `InterfaceOutbound` | An artifact left toward an interface a checkpoint names as outbound. |
| `ArtifactMoved` | A plain artifact movement between two interfaces. |
| `MarkOutcomeAdmitted` | A verifier passed; the named mark is now a fact. |
| `VerificationFailed` | The artifact reached the checkpoint and its verifier did not pass. |
| `DeliveryRefused` | The gate turned the movement back, with a typed reason. |

Refusal reasons are a closed vocabulary: `no_binding`, `policy`,
`verification_failed`, `unknown_interface`, `unknown_run`, `stale_epoch`,
`checkpoint_spent`, `settled`, `malformed`.

A refusal is a movement like any other - logged and replayable. Asking for more
than your place allows is an observable event, not a silent drop.

Live streams - thinking, progress, liveness - are projections over this log.
They are not a second source of truth.

Every row carries a trace. Traces map back to the originating interface and
the goal the work belongs to.

### Reading a movement in the client

The optional desktop client's **Dashboard → Movements** is one reader of this
record. Expand a row to see the from/to interfaces, trace, and artifact
reference alongside its manifest and payload reference. The selected
`DeliveryRefused` below records a refused transfer; neighboring arrival or
outbound rows describe other edges, not success for this one.

![Movements with a DeliveryRefused row expanded beside its artifact and payload references](../images/client/movements.png)

This is the visible counterpart of the
[payload, manifest, and metadata example](ARTIFACTS.md#payload-manifest-and-delivery-metadata).
Another client can read the same ledger and indexes.

## Indexes

Indexes make the log fast to query. They never replace it. The design copies how an operating system holds indexes: the kernel
keeps in-memory caches over the data on disk, rebuilt at mount and updated per
write, and the searchable index is a separate, disposable file beside the data.
A database is a userspace application on top of the filesystem, never part of
the kernel.

### Layer 1 - the kernel index

In memory, read-through. The arrival path's idempotency questions - has this
exact checkpoint generation already closed its required set, has it already
admitted this exact artifact, is this arrival already recorded - are answered
from an in-memory projection of arrival rows.

A checkpoint generation is `(run, checkpoint, epoch)`. A generation without
requirements is a standing route: it admits each distinct artifact once, keyed
by generation and artifact id, and is never re-armed for the next arrival. A
generation with requirements is spent when its set closes; re-arming it
requires a new checkpoint id or a new epoch. The index holds a watermark and catches up
incrementally before answering: the first question after boot folds the whole
ledger once; every later question folds only the delta. No arrival ever rescans
the biography of the world.

- One index pairs with one ledger for the process lifetime. A shrunk or
  non-contiguous log is a typed refusal; an arrival never answers from a
  phantom cache.
- A required arrival counts as admitted only when one exact, causally later
  `MarkOutcomeAdmitted` proves it. One mark proves one pending arrival;
  indistinguishable pending arrivals fail closed instead of sharing proof.
- Kernel decisions consult this layer only. Never SQLite.

### Layer 2 - the Spotlight index

`ledger/ledger-index.sqlite`, beside the log. One row per movement, with no
payloads - the sequence number addresses the authoritative line. Indexed by
run, endpoint, owning interface, artifact id, admitted mark, and kind. WAL
mode: one writer, any number of readers.

Who it is for - readers, in any language:

- an interface catching up on **its** movements from its own cursor, as one
  indexed lookup instead of parsing the whole log. The cursor itself stays the
  interface's own file; it never lives in the index.
- the client, rendering run summaries and movement history.
- a Python or JavaScript process, which opens SQLite natively.

What keeps it a cache:

- **Disposable.** Delete the file and nothing is lost. The next boot verifies
  the cache against the ledger and rebuilds it from zero on any mismatch. A
  different schema generation is dropped, never migrated.
- **Never blocks the ledger.** If ingest fails, the writer stops touching the
  broken cache, the arrival and its append proceed, and the next boot rebuilds.
  Fail-closed for truth, fail-open for cache.
- **Never consulted for a kernel decision**, and never written except as a
  projection of rows already appended.

Provider recovery validates movement projections before selecting complete grants.
Writer history uses the SQLite edge index to locate its declared call and result
transfers, then verifies their bytes, grants and ordered preimages against the
ledger. Selection includes transfers missing runtime metadata so they still fail
validation. The indexed boundary is checked against the log; an unavailable or
unusable index reads the authoritative prefix, and later rows are read from that
boundary. Complete grants are materialized only for the writer's own call and
result edges. The provider handoff and its authoring-result consumer share
one writer-history cache for the same interface and ledger. Result consumption
validates that identity and checks the appended delta against the retained cursor.
Retained delivery uses a read-only projection for its emitting interface. Its reader
checks every ledger row's JSON and sequence, then decodes only that source's movements.
Selected required arrivals keep complete grants and exact mark and replacement proof;
other interfaces' grants are outside this projection. The projection accepts only its
source and increasing original sequence numbers, and exposes retained-delivery queries,
not kernel admission decisions. The kernel's complete arrival index is unchanged.
Provider recovery checks indexed row ranges before applying them and decodes
one movement at a time. Foreign run-edit candidates retain ledger sequence
numbers until their call and result evidence is needed. The size of the scan
therefore does not determine how many complete grants must be in memory.
Other recovery readers validate their selected referenced grant content without copying
unused authority into every historical movement.

## Caches over the JSON on disk

The ledger is one truth surface. The descriptor and run JSON on disk is the
other, and the same discipline applies: the arrival path answers from an
in-memory projection refreshed only where a file changed.

**The run-record index.** A reverse map from interface folder to the active
checkpoints that pin it. Before resolving targets, the index compares each file's
identity, size, modification and change times. Settled, unchanged files reuse the
parsed record and routes. Only changed or recently observed files are reread;
equal bytes avoid reparsing. Vanished files are dropped. Malformed or unreadable
records contribute no targets, while healthy siblings continue. Admission still
reads the chosen run and forms its grant from current data.

When a run file changes, already-admitted outputs for its newly visible
bindings are replayed through the ordinary per-edge gates. A repaired recipient
catches up while the runtime stays up, with no resend, no poller, and no
delivery queue.

**The registry snapshot guard.** A walk folds paths, metadata and cached content
digests into a fingerprint. Changed files and files with recent or unusable
timestamps are reread; settled, unchanged files reuse their digests. Cache age
alone cannot change the fingerprint. Parsing and validation run when the
fingerprint changes. Before/after
observations must agree. Unreadable or stably malformed descriptors fail closed;
concurrent writes return `RegistryUnstable`.
Retained delivery preserves that typed refusal, the artifact and delivery identity.
It does not repeat the producing effect. Policy refusals remain terminal.

The one-second freshness window covers equal-size writes within a coarse
filesystem clock tick. An observation must already be settled before reading:
a cached recent observation cannot become reusable just because time passes.
Missing or future timestamps cannot establish freshness. Platforms without a
change timestamp keep the read path. Context-artifact materialization follows
the same rule before reusing its canonical blob.

## Restart

A run is rebuilt from its record and the
movements recorded against it. A stopped process loses no state.
