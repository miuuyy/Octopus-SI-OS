# Bound context collections

This interface-owned package handles ordinary `context.collection` artifacts.
The calling room establishes the exact bound source and its read grant. Entry
scope only narrows that authorized source. Explicit entry reads can return the
collection's authored content; external artifact bodies require their own
bindings. Requirements, evidence references and selection seeds are never
followed during collection discovery or read.

```rust
BoundCollection::open(
    registry_artifact_root: &Path,
    source: &ArtifactRef,
    bound_payload: &serde_json::Value,
) -> Result<BoundCollection>

collection.list(scope: &ScopeContext, request: &ListRequest)
    -> Result<CollectionPage>
collection.read(pin: &CollectionSelection, scope: &ScopeContext)
    -> Result<EntryRead>
collection.resolve_metadata(pin: &CollectionSelection, target: &ScopeContext)
    -> Result<EntrySummary>
collection.prepare_publication(
    entry: &CollectionEntry,
    expected_revision: Option<ContentHash>,
    caller: &ScopeContext,
) -> Result<PublicationPlan>
collection.records() -> Result<Vec<CollectionRecord>>
collection.directory() -> &Path
```

`records` returns every validated entry of one complete bounded scan with its
revision, whatever its state or scope. The caller that asks for it applies its
own visibility rule before showing anything; the memory tools use it to read
by EE lineage.

`parse_descriptor` and `parse_entry` validate payloads. `entry_revision` hashes
canonical whole-entry JSON. `normalized_selection` supplies empty omitted seed
arrays/maps without choosing capabilities or validating their ordinary run
contracts. The opaque selection accepts exactly `CurrentRunSelection` field
keys; the later author must validate every seed and root through its existing
owners.

`read_descriptor(root: &Path, source: &ArtifactRef) -> Result<Value>` lets an
author with an already attested bound/readable source load its descriptor
without duplicating filesystem readers. It uses the same no-follow IO,
checks schema/folder identity and canonical source hash, and reads neither
entries nor referenced bodies. Descriptor IO is limited to 256 KiB. The helper
does not establish a source grant.

The source descriptor has `label: "context.collection"`, `schema_version: 1`,
an exact `id`, a nonblank `description`, optional nonblank guidance `text`,
`directory: "entries"`, and positive `limits`: `max_entries`, `max_entry_bytes`,
`max_page_bytes`, `max_scan_bytes`. Aggregate scan bytes may not exceed 64 MiB.
A typical source uses 4096 entries, 256 KiB per entry, 64 KiB per page and
16 MiB per complete scan.

Storage is derived exclusively as
`registry_artifact_root/<source.id>/entries/<entry.id>.json`. The source's bound
canonical hash must match both its supplied payload and installed descriptor.
Only a missing `entries` directory means an empty bootstrap collection. Invalid
rows, nonregular carriers, symlinks and over-limit collections fail closed.
Handle-relative no-follow IO currently supports macOS and Linux; other hosts
return `UnsupportedPlatform` rather than use weaker path IO.

An entry requires version 1, exact local `id`, nonblank `title`, `description`
and `applicability`, `state: "active" | "withdrawn"`, `scope`, and `selection`.
Optional `content` contains nonblank `text` and/or `evidence: Vec<ArtifactRef>`;
optional `requires` lists exact existing registry-family `{kind,id}` references.
An omitted EE/worker restriction admits the authorized source; an explicitly
empty restriction denies all. No entry-authored source or path exists.

`ListRequest` contains optional explicit metadata `query`, `limit: u32` and
optional `{snapshot,after}` cursor. Query is a case-insensitive substring of
the entry's id/title/description/applicability, never a task classifier or body
search. The page carries `has_more` and a cursor only when another visible
matching row remains. `snapshot` pins source, query, caller scope and only
visible matching whole-entry revisions. Hidden row activity cannot invalidate
that cursor, while every operation still checks the complete bounded source.

`CollectionSelection` is `{source: ArtifactRef, entry_id, revision: ContentHash}`.
Revision and cursor snapshot serialize as lowercase hexadecimal; `ArtifactRef`
uses its existing native serialization. That native `{id, content}` object is
the one representation of a collection source: every `context.*` result shows
it (discovered sources, page sources, pins) and every `context.*` tool accepts
it as `source`, so a pin passes to `context.read` or run authoring verbatim.
The room resolves the source by id among the invocation's admitted bindings
and refuses a source whose content differs from the bound one.
`EntryRead.entry` preserves exact validated JSON so its canonical hash matches
the selection pin. List/metadata results omit content. Hidden or withdrawn selections are refused before
reporting a revision mismatch. Failed entry integrity exposes a generic
`SourceUnavailable`, without private filenames or metadata excerpts.

`PublicationPlan.file_write` serializes directly as the existing ordered
`file.write` arguments. Each target carries its exact complete postimage,
raw-byte preimage hash and complete preimage, or explicit null preimages for
creation. The package only prepares; it creates neither directories nor effects
nor receipt state. The existing file writer is the sole mutator and authority
for replay/receipts. Preparation refuses an old entry outside caller scope
before returning preimages. A new entry can declare another target scope;
the caller already owns the submitted content and still needs actual writer
path authority.

Preparation checks capacity against its observed bounded source. Its plan is
not an atomic multi-entry capacity reservation: simultaneous new ids may pass
the same count check. Subsequent scans refuse an over-limit source. Exact entry
preimages prevent overwriting a concurrent revision; cross-file admission and
write authority remain existing authoring/writer contracts.
