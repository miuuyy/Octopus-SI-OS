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
   quiet period) starts an ordinary task in
   Home's environment: it lists the notes with their usage, merges duplicates,
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
model the same payload.

An observer's final text is one JSON object. Malformed JSON produces an explicit
`memory.capture.error` diagnostic with the original terminal artifact reference.
That diagnostic has no capture-success mark. It settles its source so the
projection can process the next observation; it never extracts fenced JSON or
rewrites the model's decision. Well-formed outcomes still cross the declared
capture verifier before any success mark is admitted. An outcome that verifier
refuses is settled the same way: its replacement is a `memory.capture.error`
diagnostic that keeps the model's decision and names the refusal movements, so a
refused outcome is never resubmitted unchanged. The diagnostic is an ordinary
projection output, not part of the projection run's session snapshot. A refused
final changes its label only this way: from a verified output to a declared
output that requires no mark, with the refusal evidence as its proof.

## Remembering is one call

The model never works out which JSON file to write where. The **Memory** plugin
(Applications → Plugins, included in the Codex pack) gives Home and tasks four
tools behind the `system_memory` door:

| Tool | What it does |
| --- | --- |
| `memory.save` | Store one note. Optional `title`; optional `supersedes` withdraws earlier notes of the same environment in the same write. |
| `memory.search` | Keyword search over the notes this run can see, ranked by hits, with previews and ids. |
| `memory.read` | Open one note in full; records that it was used today. |
| `memory.list` | Page through visible notes with their usage; `include_withdrawn` for maintenance. |

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

The registry ships `skill.capture`, `skill.upsert`, `skill.search`,
`skill.list`, and `skill.get` as **ordinary tools** behind the system room's
door. A concept-named tool is fine. A concept-named runtime primitive is not.

- **Authority is policy, never worker identity.** Whoever's formed grant
  includes the tool may call it. A memory worker is the typical writer in a
  maintenance environment because policy grants it there. That is
  configuration, not doctrine.
- **Visibility is binding.** A worker receives a concrete skill artifact
  through an artifact binding, or searches when policy grants the tool. Search
  narrows within binding-gated access; it is not a global lookup.
- **Runs carry provenance.** A skill is written from inside a run, so its
  source run and trace are honest by construction. Needing to fabricate
  provenance is the signal you are off the road.

### Three ways a skill gets written

1. **Seed.** Any run with the grant writes it directly. Hand-written seeds are
   first-class.
2. **Capture.** A worker whose policy grants capture emits a proposal artifact.
   What happens next is ordinary artifact movement: a granted worker may
   inspect it, refine it, reject it, or persist it.
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
