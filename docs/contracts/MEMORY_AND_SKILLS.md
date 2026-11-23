# Memory, skills, and persona

Memory, skills, and persona are artifacts bound to runs. Updating one is an
ordinary run under policy.

The shared worker-role artifact identifies the installed system with the agent:
its goals, runs, interfaces, triggers, worker environments and their connections
are Octopus, and a run is one part of its work. Maintaining and improving that
system is part of the role. Home and Task role artifacts specialize it; provider
choice does not change the identity. This guidance is bound context, not
additional authority or evidence of work elsewhere. The realtime provider's
session instructions present the same identity through voice.

The system library's `self-improvement` skill carries the detailed guidance.
Its catalogue applicability covers substantial or long-running work, continuing
intentions, dependencies, recurring work and observed friction. The model reads
the entry through the ordinary context collection tools when relevant. It builds
and maintains a bounded picture of current goals, runs, connections, capabilities
and results; questions the organization of the work; and maintains, repairs or
extends the system within its grant. Improvements include adding missing
capabilities and connecting them to their consumers. The shared role keeps a
short responsibility statement and pointer instead of repeating this procedure.
Skill selection remains a model decision; no task classifier or execution loop
is added. Working state summaries remain observations over owning records and
recorded effects, carried through declared artifact routes when needed.

## Memory is an artifact

Memory **state** is an artifact made visible through bindings. A memory
**change** is a run.

Retrieval is not a free global read. A run retrieves only what it holds a
binding for. "Give me relevant memory" is an artifact binding, not a
side-channel database query and not a memory-specific runtime interface.

An ordinary run artifact is not automatically memory. It becomes memory, in the
sense a person would mean, when it is bound into a run's context.

Memory is written in two ways:

1. **Directly, during work.** When the user says "remember this", corrects the
   model, or a procedure is proven, the run makes one `memory.save` call.
2. **In the background, after work.** When a run settles, an observer run reads
   a bounded projection of what happened and maintains the same collection
   through the same tools: it searches first and saves one note per fact,
   superseding what a correction replaces. Its memory door names the observed
   run, so the observer can live in a narrowed services environment while its
   notes take the observed run's place. A verifier admits its outcome only
   when every save it names is scoped to the environment the evidence shows for
   the observed turn and landed in the collection. After a quiet period, the **Memory consolidation**
   trigger (recommended in the Codex pack, armed by setup with a three-hour
   quiet period) wakes its consolidation task, which setup created and bound to
   the trigger in Home's environment: it lists the notes with their usage, merges duplicates,
   settles contradictions using their sources and the user's latest applicable
   decisions, and folds stale, unused notes into what is still true. A newer
   note is not automatically stronger evidence. Both are ordinary runs started by
   interfaces.

The resulting entries become visible only to runs whose bindings and formed
policy admit them.

Knowledge entries retain their sources and distinguish observations, proposals
and inference. Descriptions include conditions or unresolved dependencies that
affect whether the knowledge is usable. Useful failures and partial results can
also be retained. Capture and consolidation preserve these distinctions and the
entry's audience.

The projection's byte limit covers the complete artifact, including its model
text and any original input attached after source verification. When it is too
large, the package removes older optional evidence, counts omitted references,
and shortens selected fields with explicit truncation metadata. The exact input,
terminal and provenance references remain intact; an envelope whose provenance
alone exceeds the limit is refused. The field-trimming flag describes only fields
still present in the selected input, its original input, terminal text and retained
evidence. Removing an evidence entry updates the separate entry-omission count;
its field flags do not survive the entry. The verifier checks every retained
trimming record against its own field container.

An observer outcome carries complete calls and results for its declared
publication tool, including publications absent from the model's receipt list.
Other context entries remain exact references in the omission record. The
projection verifies their source evidence before selection and never trims
publication proof to meet its output bound.

A projection window starts at its selected input and follows the session's
consumption order. An input admitted while the source turn was working is
consumed after outputs that entered the ledger later, so evidence after the
selected input may carry an earlier movement than its neighbour. The window
refuses only evidence older than the selected input and duplicate references.

Any input that can start a turn of the observed run can be the selected input.
An operator message gives its declared fields. An artifact admitted from one of
the run's external inputs, such as mail or a sensor event, is selected whole:
each top-level member except `session_context` becomes a field, because its
shape belongs to the interface that produced it and the provider showed the
model the same payload. The observer's projection declares the source's input
routes and rules; a current-run amendment that adds an input to the source
re-forms them in the same transaction, so a trigger bound later is captured
like one bound at creation. It re-forms them from the observer selection the accepted declaration
records, so the routes follow even after the Home stops selecting an observer
for new tasks. Removing an input keeps the rules and routes the projection
already declared: a turn that arrived through the removed input before the
edit is still selected as itself, while the source no longer pins the route,
so nothing new can cross it.

An observer's final text is one JSON object. Malformed JSON produces an explicit
`memory.capture.error` diagnostic with the original terminal artifact reference.
That diagnostic has no capture-success mark. It settles its source so the
projection can process the next observation; it never extracts fenced JSON or
rewrites the model's decision. Well-formed outcomes still cross the declared
capture verifier before any success mark is admitted. An outcome that verifier
refuses is settled the same way: its replacement is a `memory.capture.error`
diagnostic that keeps the model's decision and names the refusal movements, so a
refused outcome is never resubmitted unchanged. A projected experience input
that the input verifier refuses settles the same way, as a `memory.capture.error`
diagnostic naming the refusal movements. The diagnostic is an ordinary
projection output, not part of the projection run's session snapshot. A refused
final changes its label only this way: from a verified output to a declared
output that requires no mark, with the refusal evidence as its proof.

A terminal the projection cannot project — an input outside its declared
routes, a missing required field, an exceeded bound — is settled the same way:
the projection hands a `memory.capture.error` diagnostic naming the error and
the terminal, the observer model is not woken, and the next observation is
processed. One unprojectable source never stops the projection.

## Remembering is one call

The model never works out which JSON file to write where. The **Memory** plugin
(Applications → Plugins, included in the Codex pack) gives Home and tasks five
tools behind the `system_memory` door:

| Tool | What it does |
| --- | --- |
| `memory.save` | Store one note. Optional `title`; optional `supersedes` withdraws earlier notes of the same environment in the same write; optional `corrects` names notes the run can see but cannot replace. |
| `memory.search` | Word search over the notes this run can see, ranked by BM25, with previews, usage and ids. |
| `memory.read` | Open one note in full; records that it was used today. `include_evidence` adds a bounded description of the call that saved it. |
| `memory.list` | Page through visible notes with their usage; `include_withdrawn` for maintenance. |
| `memory.inspect_evidence` | Trace one note to the call that saved it, with a bounded preview; not counted as a use. |

**Ranking.** A note and a query are read as words: maximal runs of Unicode
letters and digits, lowercased. A query word that equals a word of the note is
scored with BM25 (`k1` 1.2, `b` 0.75) over the notes the caller can see, a title
word counting twice. A query word of three or more characters that occurs only
inside a longer word is a partial match. Every whole-word match ranks above
every partial one, so `port` finds the note about a port before the notes about
a report or a portal, and a word that starts with the query word ranks above
one that only contains it. Equal ranks list the newer note first. Each match
carries its `score`, its `match` kind (`word` or `partial`) and its `usage`
(`days_used`, `last_used`).

**Corrections.** `supersedes` withdraws a note, so it works only on notes of the
saving run's own EE. `corrects` leaves the named note as it is: the note must be
active and visible to the saving run, and the save writes one receipt per
corrected note at `corrections/<corrected id>/<correcting id>.json`, beside the
collection's `entries` and `usage`. Entries keep their shape, and a receipt is
only ever added. The conflict is resolved when a note is read: `memory.read`,
`memory.search`, `memory.list` and `memory.inspect_evidence` give every note a
`corrected_by` list of the active notes visible to the caller that correct it,
each with its id, revision, title, `saved_at` and `scope_ee_ids`. A correcting
note the caller cannot see, or one that was withdrawn, is not listed.

**Evidence.** A note records the call that saved it as its evidence.
`memory.inspect_evidence`, and `memory.read` with `include_evidence`, take the
id of a note visible to the caller, never an artifact id, and follow only the
references that note records. A reference is described only when it is the
note's own saving call, a `tool.call` whose `arguments.note` is the note's text;
any other recorded artifact is reported as `withheld`, so an edited evidence
list does not make the door a reader of unrelated artifacts. The call is read
from the content-addressed blob store through an optional read grant on
`artifact_blobs_root`, and each blob is checked against the hash that names it.
The description gives the tool, the correlation id, the origin the provider
recorded beside the arguments, the payload's hash and size, and a preview of
at most `max_bytes` (2048 by default, 8192 at most) cut on a character
boundary. A missing grant, a missing blob or a blob that differs from its hash
is `unavailable`. Host paths are not returned. The movement log is not read, so
the description carries no movement sequence; the note's `saved_at` is the time
of the save.

Behind them, the system room's own executable does the artifact work. The door
names its collection in its configuration (`memory_collection`); the shipped
door uses the installation's shared `artifact-system-library` collection. A
save becomes one ordinary collection entry:

- **Scope** is the calling run's EE, or, on a door whose configuration names
  the run it serves (`observed_run`), the EE in the latest bound
  `memory.experience.input`. The tool checks its exact admitted reference,
  read grant, source run and collection before reading or writing memory.
  Missing experience refuses the call; current placement is not substitute
  evidence. A run sees notes of that EE and the EEs above it, never a sibling's.
- **Memory follows place**: a note keeps the scope it was saved under. When a
  run moves, its earlier notes stay with the old EE and later notes take the
  new one. An observation already in flight keeps its evidenced EE through
  the save, even if the run moves meanwhile. Nothing migrates notes behind the
  user's back.
- **Provenance** is the invoking tool call, kept as the entry's evidence, so the
  ledger names the run, trace and checkpoint that saved it.
- **Deduplication**: saving a note identical to one already visible returns the
  existing note.
- **Policy**: saving needs the `create` artifact verb and the collection bound
  for reading; superseding needs the `change` verb and only reaches notes of the
  caller's own EE. Anything else fails closed.
- **Durability**: a new note is installed exclusively, whole or not at all; a
  superseded note is replaced by an atomic exchange that is undone if the note
  changed meanwhile. The plan is kept per call, so a retried call ends in the
  same notes. A retained plan with a different EE scope is refused before it
  can publish another write.
- **Usage**: each read leaves one dated receipt per note per day beside the
  entries; tools report `days_used` and `last_used`, and consolidation uses them
  to decide what to keep.

The read path mirrors Codex's: a compact catalogue of visible entries is bound
into a run when it is authored, a short guide artifact tells the model when to
consult memory and when to save, and details come on demand through search and
read. Notes are information, not instructions.

## Skills are artifacts

A skill is an artifact. Some are written by hand at setup time and then
maintained by the system. A "how the system architecture works" artifact is
bound into almost every worker's context for grounding. A coder additionally
binds a "how the codebase is laid out" artifact.

After a successful run, another run can notice the world changed - a module was
added - and update the relevant skill so the next run sees the new reality.
That update is an ordinary worker run editing an artifact. It is not a skill
subsystem.

Role and skill artifacts explain their purpose, the relevant entry points and
where to read exact contracts. The model follows the affected producers,
consumers and related state, adapting its investigation and verification to the
task. Schemas and detailed operation examples remain available on demand;
general instructions do not repeat them or prescribe a universal checklist.

The system library's `framework-authoring` entry carries portable Octopus
development guidance: architecture principles, the four development axes and
engineering discipline. The system-review trigger binds that library, and its
role directs the run to read the entry before changing Octopus. The guide does
not depend on a maintainer's private skill files or grant additional authority.

The registry ships bound context-collection tools (`context.discover`,
`context.read`, `context.prepare_publication`) and ordinary artifact tools
(`artifact.read`, `artifact.list`, `artifact.search`, `artifact.append`,
`artifact.send`) behind the system room's doors. A concept-named tool is fine.
A concept-named runtime primitive is not.

- **Authority is policy, never worker identity.** Whoever's formed grant
  includes the tool and collection binding may discover, read, or publish
  entries. That is configuration, not doctrine.
- **Visibility is binding.** A worker receives a concrete skill or library
  collection through an artifact binding, and searches only within its bound
  collections. Search narrows within binding-gated access; it is not a global
  lookup.
- **Runs carry provenance.** Every published entry and memory note is written
  from inside a run, so its source run and trace are honest by construction.
  Needing to fabricate provenance is the signal you are off the road.

### Three ways a skill gets written

1. **Seed.** Any run with the grant writes an artifact or publishes a collection
   entry directly. Hand-written seeds are first-class.
2. **Capture.** A run observes a reusable procedure and either saves it via
   `memory.save` or prepares a collection entry via `context.prepare_publication`
   and `file.write` (or sends a proposal artifact to a review run).
3. **Delegation.** A run that should not hold write grants emits an artifact
   through a System-interface binding to a run that does.

## Persona is the same shape

Persona is an artifact bound to a run, and it can differ per run. The Home run
holds a binding to a persona artifact. It starts essentially empty and it is
mutable: changing it means running a worker in the right environment, under
policy, that edits the artifact like any other.

## The two-layer rule

Memory and skill behavior belongs in registries and owner packages. Authorized
runs may repair runtime source under the workspace grant and development rules,
preserving the kernel's generic contracts.

- Skill-named or memory-named **registry objects are fine**: tool descriptors,
  artifact descriptors, EE templates that allow those tools.
- Skill-named or memory-named **engine code is the violation**. If the runtime
  knows the word "skill", a registry concept has been hardcoded into the runtime
  and can no longer be evolved from the registries.

## Native emergence

Do not solve a missing behavior by creating a module named after it.

If skills are not being created, the answer is not a skill module. Inspect the
native path: a finished run → an experience artifact → policy permitting its
transfer → arrival at a receiving checkpoint → a memory worker transforming it
→ an updated artifact visible through permitted bindings. If that path is
broken, fix the primitive or the contract that broke it.

If self-improvement is not working, the answer is not a self-improver.
Self-improvement is the system using its model of itself to create goals, runs,
interfaces, or descriptor changes when policy permits. Those are framework
operations, not a feature loop.
