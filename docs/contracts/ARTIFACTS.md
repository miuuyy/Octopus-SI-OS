# Artifacts

Artifacts are the universal data carrier. Every piece of data that enters the
framework through an interface arrives as an artifact; every piece that leaves
departs as one. Inputs, outputs, memory, tool calls, tool results, evidence  - 
all artifacts. They are not a sub-category of "run data". They are the
primitive.

When artifacts stop moving, nothing meaningful has happened in Octopus. Hidden
process state, handler-local memory, or provider fields that never appear as a
movement are not replayable architecture.

## What crosses the boundary

The boundary between an interface and the runtime is an artifact - not a
private request lane and not a handler-specific payload. What crosses names:

- **a self-identifying name** - the emitting interface's own vocabulary, not a
  runtime type field;
- **a reference** to where the payload lives, with its content hash;
- **delivery metadata** - the movement and invocation context needed for replay
  and projection, carried alongside the artifact reference.

The runtime records that reference as movement evidence and fails closed on a
malformed, unreadable, or hash-mismatched one. At hand-in the boundary captures
the manifest and payload bytes; admission pins both by content hash in the
durable blob store before recording the movement. Later readers resolve the
pinned bytes, not the original staging paths. The emitting interface owns its
staging files and may reclaim them after admission and its own durable completion,
provided no prepared or pending work still needs them. The activation transport retains the staged manifest through the
delivery verdict. Admission releases its private staging copy. Refusal retains
the manifest, and the sender continues to own its original payload.

This shape is the only coupling between an interface and the runtime. A
terminal UI, a provider bridge, an HTTP poller, a microphone adapter, and the
desktop client construct entirely different payloads and all cross the same
way. Runtime code must not branch on a product's interface name or accept a
special path beside artifact movement.

## Payload, manifest, and delivery metadata

The useful network analogy is **payload plus delivery context**. A packet has
content and information about its delivery. Octopus separates these concerns
too, but its payload can stay in storage while a reference crosses the boundary.

| Part | Example | Who supplies it |
| --- | --- | --- |
| Payload | Transcript text, image bytes, a tool result | The emitting interface. |
| Manifest | `label`, `payload_path`, `payload_content_hash` | The emitting interface; the boundary validates and pins it. |
| Movement | `from`, `to`, `trace`, sequence and time | The runtime's ledger records the exact crossing. |
| Invocation metadata | `runtime`, `grant`, bound artifact refs and checkpoint requirements | The runtime supplies the admitted invocation context. |

![A Codex CLI reply crosses the provider and operator interfaces under bindings and policy; payload, artifact manifest, and invocation metadata remain distinct](../images/architecture/artifact-boundary.png)

This illustrates one reply crossing. The receiving interface reads its recorded
movement from the ledger and resolves the payload reference. Codex CLI is code
behind the provider boundary; another provider uses the same artifact contract.

For example, a speech interface produces the UTF-8 text `Read README.md.`
(without a trailing newline). Its manifest can be:

```json
{
  "label": "speech.transcript",
  "payload_path": "/absolute/private-state/transcript.txt",
  "payload_content_hash": "adead9430e5975db422a527b3b0a3cd412f6d24eb8dbd6c19e0473980ef9e07f"
}
```

The path and label are illustrative; the hash is for those exact text bytes.
An audio example uses the same manifest shape with a reference to audio bytes.
The binary media need not be embedded in JSON.

When that artifact reaches a bound provider, the decoded invocation can
include the following context. This is an excerpt with illustrative ids and
only selected grant fields, not a complete persisted ledger row:

```json
{
  "runtime": {
    "run": "run_home",
    "ee": "ee_existence",
    "worker": "system_worker_operator_home",
    "checkpoint": "cp_read",
    "interface": "world_provider_codex",
    "epoch": 0
  },
  "grant": {
    "goal": "Explain README.md",
    "tools": ["workspace.read_file"],
    "interfaces": ["world_provider_codex", "system_workspace_env"],
    "worker_environment_kinds": ["filesystem_workspace"]
  }
}
```

The provider can now distinguish **what arrived** from **what this run may do
with it**. It projects the bound context and grant into the model's prompt and
tool list. Adding `"grant": { ... }` to the transcript would grant nothing:
payload text cannot supply trusted invocation authority.

There is no free-form `metadata` field on the manifest above; unknown manifest
fields are rejected. An interface can define descriptive data inside its own
payload, but that is separate from runtime metadata. A model's permissions
travel in the invocation envelope, not in a policy artifact or a special
metadata binding.

One screenshot delivered to two checkpoints has the same content identity and
two distinct delivery contexts. Each edge gets its own grant and verdict. Like
a network address, the exact interface identity determines the boundary; a
label such as `speech.transcript` describes content and does not select a route.
No network connection is required for a local crossing.

For storage, the ledger retains the grant projection by content reference
(`grant_content`) and readers reconstruct the invocation's `grant`. That is
evidence of the invocation, not a saved grant to trust on the next arrival.
See [the ledger](LEDGER_INDEX.md) and [policy formation](POLICY.md).

## There is no runtime `kind`

Per movement the kernel sees three things: the route (the binding), the caller,
and the movement itself. It never admits, rejects, or routes by payload type.

- **Self-identification.** An artifact identifies itself by name and body  - 
  `operator.input`, an `.mp3` path, a `tool.call`. That vocabulary is private to
  the interfaces that speak it. Two interfaces may name things entirely
  differently and the kernel never knows.
- **Admission is the receiver's job.** What an interface accepts is checked by
  the interface's own code against its own descriptor, fail-closed. There is no
  allowed-kinds list read by the kernel.
- **Policy granularity rides the binding, not the payload.** "This run may only
  read here" is a grant on the binding. The kernel forms it by intersection and
  hands it to the room in the invocation envelope. The room enforces it, and
  since the room is also the executor, the check cannot be bypassed.
- **The kernel has no control artifacts of its own.** Run records, goals, and
  EE descriptors are JSON files. Changing one is an ordinary file-write tool
  call in a room whose path covers it. Creating one is writing a new file the
  same way. The evidence is the ordinary gated tool-call movement.

The runtime's one verb is
an artifact arriving and an artifact leaving. Session semantics such as resume
or archive belong to the interface that owns the session; the kernel never
learns what "resume" means.

Formats are the interface's private business. A text-to-speech output is an
audio artifact whose manifest points at an MP3. The runtime moves it without
understanding MP3. A speaker interface plays it only if its own code supports
that carrier.

## Fan-out

An arriving artifact fans out to **every active checkpoint, in any run, that
pins the arriving interface folder and can take it**. A checkpoint can take an
artifact when its inbound binding admits the artifact's name and, if it
declares required verified artifacts, one of them names it. One movement edge
per such checkpoint, each with its own verifier outcome. No sender claim picks a
single winner. When checkpoints pin the folder but none can take the name, the
hand-in is refused as `policy.interface_binding_denied`.

A checkpoint without requirements is a **standing route**: each distinct
artifact enters its generation once, and the next one enters the same
generation. An ordinary input or output keeps admitting arrivals without the
run being rewritten. A checkpoint with requirements is a one-shot wait: it
closes its required set and is spent.

The scope is the folder, and the rule is the same for World and System
interfaces. Isolation comes from identity, not from a reach rule: a separate
conversation is a separate interface folder, so nothing leaks between threads
without anyone narrowing the fan-out.

**Every edge carries its own verdict.** A refused edge - a spent checkpoint, a
repeated artifact, a policy denial, a failed verifier - is recorded as that edge's own typed
movement, and sibling edges deliver regardless. The operator's branch never
suffers for a tool room's failed check. A hand-in as a whole is refused only
when it admits zero edges.

## The one way a model chooses an address

Beside authored outbound bindings there is one way a model picks a
destination: the `artifact.send` tool. The room's door materializes the payload
as an ordinary artifact and hands it to the runtime addressed at an authored
System-interface binding. The gate re-forms the **emitter's** grant from its
own run record and checks that pair. A refusal is a typed movement
(`no_binding` or `policy`). An admission is one `InterfaceOutbound` row and
then the ordinary fan-out.

An admitted emission with no live reader is a success, not a refusal. The
recorded movement is the fact, and a sleeping reader catches up from its
cursor. The tool's response is the delivery verdict of the hand-in, never a
receiver's reply.

## Bindings are the visibility boundary

A worker does not get memory, skills, or persona by global read. It sees
artifacts delivered through its active bindings. Each interface binding may
narrow that with an `artifact_policy`:

```json
{
  "interface_binding": {
    "interface": "world_operator_chat_voice_home",
    "config": {
      "artifact_policy": {
        "mode": "allow",
        "names": ["realtime.session.offer", "realtime.session.stop"],
        "namespaces": []
      }
    }
  }
}
```

That is binding data enforced at the boundary, not a kernel classifier. An
artifact that does not satisfy it is refused with
`policy.interface_binding_denied`.

A checkpoint's `artifact_bindings` list the context artifacts the run reads  - 
roles, grounding, skills, memory. The checkpoint layer does not need to know
whether a referenced payload is JSON, audio, a vault directory, or a session
log. It carries the reference; the interface on the active binding decides
whether it can materialize that carrier.

## Artifact tools

These are ordinary tools behind the system room's door.

| Tool | What it does |
| --- | --- |
| `artifact.append` | Create or append an artifact payload and return a content-addressed reference. |
| `artifact.read` | Read a text surface from an artifact already bound to the run. |
| `artifact.list` | List the artifacts bound to the run. |
| `artifact.search` | Search the text of artifacts bound to the run. |
| `artifact.send` | Move an artifact into one authored System-interface binding from the run's grant. |

The tool surface is a strict boundary. Arguments are validated, not repaired. A
malformed ledger row is evidence corruption, not an empty ledger. Tools never
emit marks: if a checkpoint requires an artifact verified, the mark appears
only after the verifier passes over that reference.

Artifact search returns ranked excerpts, not full artifact payloads. It defaults
to 20 hits, retains at most 2 KiB of UTF-8 text per hit, and bounds the serialized
result body to 64 KiB. Each hit records its exact byte offset, retained byte count,
source-line extent and truncation state; `artifact.read` can retrieve the original
text at those offsets. The result also reports total and omitted matches, including
omissions caused by the result byte bound. Search never widens the run's bindings.

## Evidence

Some artifacts serve as mark evidence. A mark proves only what its evidence
artifact proves; the payload, content hash, and movement rows must be enough
for replay. Text claiming a side effect happened is not proof unless the
required evidence artifact backs it.

- The model decides which facts matter. Deterministic code only stores and
  retrieves.
- Records are append-only. A correction is a new row, not a hidden rewrite.
- A durable finding should carry its evidence: a source URL, a screenshot
  reference, a page title.
- A final answer should be grounded in artifact rows when rows exist.

## Memory

An ordinary run artifact is not automatically long-term memory. It becomes
memory when it is bound into a run's context. See
[Memory and skills](MEMORY_AND_SKILLS.md).
