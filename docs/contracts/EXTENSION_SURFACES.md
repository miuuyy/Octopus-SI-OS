# Extending Octopus

The cookbook for adding capabilities without editing the runtime.

## It should work like a package

Adding an interface, a worker, a tool, a verifier, or a room should feel like
dropping in a portable bundle and editing a few JSON bindings. "Package" is a
distribution layout, **not a runtime primitive**. Once loaded, the runtime does
not reason about package identity. It sees ordinary descriptors, exact
interface folders, bindings, artifacts, policy, marks, rooms, and runs.

For an existing family, a new Telegram-like channel, provider, or worker preset
is descriptor and binding data. If adding one channel requires editing app
bootstrap, session logic, or a manager named after the concept, a primitive has
leaked.

If a genuinely new execution family is missing, add that family once as code
inside its owner folder. Every later instance of it is data.

Installed owners may contain authorized local edits to descriptors, role
artifacts and executable code. Health checks validate their identities,
structure, declared imports, client APIs and diagnostic probes; byte equality
with a distribution or an installation receipt is not a health requirement.
Receipts retain installation provenance. Content digests remain appropriate for
immutable artifact references, release integrity and concurrent-edit protection
during publication or removal.

Octopus is a research preview. Incompatible persisted formats require a fresh
Home through normal setup. There are no schema migrations, predecessor tables,
automatic package adoption, or startup repairs of missing fields in old state.
The bundled registry initializes a new Home once; later starts use the installed
registry. Explicit package installation and recovery of interrupted operations
in the current format remain supported. Preserve sign-in credentials when
resetting a disposable Home; do not transplant old runs, checkpoints or ledgers.

## Interface process lifecycle

Life reconciliation caches resolved configurations and executable digests using
file identity, size, modification and change times. Cold resolution shares the
same digest cache; content fingerprints include the checked executable digest
instead of copying its bytes for each bound interface. Reconciliation yields
between interfaces and observes shutdown before lifting further lives.
A metadata observation less
than one second old is racy: only that file is read again, so equal-size writes
within a coarse filesystem clock tick cannot reuse stale content. The observation
must already be settled before the read starts; a cached racy observation never
becomes reusable merely because time passes. Missing or future timestamps keep
the read path. Platforms without a change timestamp also keep that path.
Build receipts still check the executable digest, and changed content participates
in the life restart fingerprint. Cache age alone cannot change that fingerprint
or restart a process. This is freshness and receipt consistency, not
a security boundary against an actor able to replace both executable and receipt.

The generic Rust life bridge acknowledges readiness publication. Awaiting its
ready signal succeeds only after the bounded Ready frame has been written and
flushed; synchronous recovery work after that boundary cannot postpone the
publication. Cancellation, a closed publisher, or a write/flush failure remains
an error. This acknowledgement covers readiness publication, not completion of
pending artifact deliveries or recovery work. An in-process life signals its
readiness observer directly.

The life host observes root exit separately from group closure. On macOS it
keeps the root waitable until the owned group closes, so cleanup never signals
a recycled PID or PGID. Protocol EOF allows a bounded exit observation; process
cleanup and private stderr draining also have finite deadlines. Wait, signal and
cleanup failures remain explicit, with the original exit status and private
diagnostics preserved. Each incarnation writes its own bounded stderr capture
in the interface's var directory; the host keeps the sixteen newest, so a life
that restarts with backoff for days keeps its recent history without filling
the disk.

## In the client

Conversation pane tabs, selection, visibility and layout are client-local state,
scoped to the Home, access profile, client window and conversation. Navigation
restores that conversation's state without writing it into a run. Reading or
opening a task does not change its position in the normal last-update order.
Creating or explicitly renaming a Project writes `name` in its EE record through
the guarded authoring transaction. Topology and Project lists read that record;
the presentation sidecar's name is only a mirror. Goal and template remain
separate fields, and an unnamed EE is shown by its ID.

Composer dictation is client-local preparation. Live speech is streamed to the
client's transcription provider and appears as a transient editor preview.
Only finalized text enters the persisted draft. Completed and cancelled audio is saved by
default under the selected Home's client data, separated by access profile,
with a bounded history of 20 recordings. Voice settings show that history
inline under Dictation: transcript, timestamp, Copy, Retry for audio without text,
and a Download/Delete menu. Composer failure recovery opens the exact recording
in these settings. Stop finalizes recognized text; the Send arrow finalizes and
submits it through the declared operator interface. Escape cancels capture or a
pending transcription while preserving manually typed text. Transcription failure
retains its audio for explicit retry; it does not silently switch recognition paths.

Project source folders are ordered client configuration in the EE's
`config.octopus_os.source_folders`. The first folder is Primary. New task
composition binds that folder and the remaining explicit roots to the selected
workspace door, and carries their names in the operator input's client context.
Full access keeps the same working context while its separate host-root grant
controls filesystem reach. Editing project folders affects future tasks;
existing private doors retain their accepted roots. Folder edits and presentation
are one guarded client transaction whose receipt proves the exact EE writes
and Home-sidecar publication.

An operator message rejected during preparation is explicitly `not_sent`.
The client restores a manual message to the composer and reports the error
through its existing error surface. An uncertain delivery is reconciled against
admitted message identity; an explicit retry retains that identity. It never
retries automatically. A newer draft is preserved, with the failed message kept
in durable client storage until it can be restored or explicitly handled.
Provider turn boundaries also split the visible history when another run starts
a new turn, so an earlier answer cannot hide later activity.

The native client checks the selected Home's writable storage before sending.
Below 16 MiB available, a native ENOSPC/EDQUOT refusal, or an unsuccessful write
probe blocks new sends with a visible explanation. The composer keeps its
current draft in memory; failed disk persistence explicitly asks the operator
to keep the window open. Queued messages do not dispatch while storage or their
exact provider life is unavailable. Observed life failures are presentation
state, not fabricated run status or ledger evidence.




The optional desktop client's **Applications** catalog presents these packages
as providers, connectors, plugins, and context artifacts. A connector such as
Slack installs interface files; making it available in a Project and connecting
it to a task are separate steps, and only connected tasks receive its messages.
Photos, screenshots and image attachments arrive inside the message artifact,
so the model sees them as images. What a connector can do (post, reply, edit,
react, upload, read a thread) is its operation catalog, the same declaration an
MCP application uses: one JSON file per action under `operations/` with its
input schema and description. The model calls each action as its own tool, and
the connector answers each request exactly once on its result label, with
images it read as `media`; an action whose outcome is unknown after a restart
is reported as `indeterminate_external_effect`, never repeated.
A plugin is a service the model calls through its interface, such as an MCP
server, together with the context artifact that teaches the model its contract.
Installation does not make the model call it automatically. An application pack groups packages for setup without
introducing a new runtime identity.

**Connect** is the client's own setup step, like signing in to a provider. An
interface can declare a connection module under its `packages/` folder in
`private_config.client_connection`; the client runs its `probe`, `connect` and
`disconnect` to check what you enter against the real service, show the
connected account and walk you through browser sign-in or pairing. A module may
live in a family owner's folder when that owner declares it, as its own module
or in its `family_modules`; the Application's package then names that owner's
package in `required_packages`, so installing it installs the module. The MCP
client package owns two such modules: `connection.cjs` for servers reached with
a key or OAuth, and `chatgpt_apps.cjs` for apps linked to the ChatGPT account,
such as Gmail, GitHub and Google Drive. Each ChatGPT app declares its connector
as data in `private_config.connector_link`: the connector id, the slug of its
ChatGPT apps page, and a read-only profile tool with the email or login field
that names the account. An application on `connection.cjs` names its account
through `private_config.account_profile`: an MCP resource that lists accounts,
or an https request to the service's own API that verifies the key. Each
connection check opens its own MCP session and ends it with HTTP DELETE.
Applications without a module are connected once their required setup fields
are saved. The account a module shows is the service account the application
works in, such as the Gmail mailbox; the sign-in it is reached through, such as
the ChatGPT account, appears only as its detail. A bot connector shows the bot
as its account (the Telegram bot's @username, the Slack bot and its workspace,
the Email mailbox). When the service is the ChatGPT account itself, as for
Image Generation, the label is that account and the detail is its ChatGPT plan.
A module reads each value the way the interface's life receives it: a value in
the launch environment wins over the saved one, a different saved value is an
error, and Connect does not save a value the environment already supplies.
Disconnect returns the state that actually remains and names what still grants
access. A module may also return a
`manage` link (a label and an https address) to the page where that account is
managed, which the client offers in the connection menu. The
connection check is presentation only: the interface still refuses to work with
bad credentials, and nothing is recorded in a run or the ledger. For an MCP
application that declares `server_tool_operations`, Connect also turns each tool
the server lists into an operation of that application, keeping the server's own
annotations (`readOnlyHint`, `destructiveHint` and the rest) in the operation.
Every Connect of an application with operations, and every publication of a
server's tools, rewrites its operation catalog artifact
(`artifact-operation-catalog-<interface>`) from the whole `operations/` folder.
Connected tasks bind that catalog, so the model sees each operation as its own
tool — including tools published after the task was created. A task cannot be
composed while the catalog is missing, so the check reports an application whose
service is linked but whose catalog was never written as not connected, and
Connect writes it.

A Project task, an Agent and Home are offered an application only when their
worker pin's ceiling holds it: every interface the application names, and each
declared task capability's tool, owner interface and Worker Environment kind.
Nobody keeps that list by hand: installing, removing or repairing a package
writes all of that for every installed application into the ceilings of the
worker pins their packages mark `application_host`, and takes out what only
removed applications declared; what the host's own source policy grants and
entries no application declares stay. Installation writes into no other worker
pin. Every worker a pack offers as the Home worker, the task worker or an Agent
preset source is such a host — the Home and task workers of Octopus Home and the
Computer Use worker of Desktop Session. A host whose installed receipt predates
its package's configuration contract refuses every package operation before
anything changes; that Home is initialized fresh. Making an application
available in a Project admits it within the EE chain and never widens a worker
pin. An application the Project admits but the worker's ceiling does not is
listed in the Agent dialog, disabled, with that reason. An Agent's pin is a copy
of its source worker pin; saving the Agent re-forms its `policy.json` from that
source, so a widened source ceiling reaches an existing Agent on its next save
and never through any other path.

Connected tasks share the application's one interface: one life, one session,
one set of credentials. An operation call goes from the task's provider to that
interface, and its result reaches only the calling task: the call arms a wait
for exactly its own result — the same operation, correlation id and the result
label the catalog declares — checked by the `tool_result_correspondence`
verifier. The interface's `private_config.task_input_labels` lists what a
connected task receives unprompted, such as a connector's incoming messages;
those start a turn, and nothing else from the interface reaches the task.
Interfaces that receive such events are never copied privately, so one poller or
socket serves every task.

Approval follows the task's access, as in Codex. A Full access task carries
`approval_policy: "never"` and no tool asks. A Project access task carries
`"on-request"`: a server tool then waits for your approval unless the server
declares it read-only, or neither destructive nor open-world. The application
answers such a call with an approval request instead of running it; the chat
shows **Allow once**, **Decline** and **Cancel**. Your answer is checked by the
`operator_approval` verifier against that exact request and recorded in the
task's evidence, and only then does the application send the call; a declined
call returns `operator_declined` to the model. The call's wait stays armed until
that final answer arrives. The same rule covers the generic call by tool name. Other calls of
the same native turn keep waiting or settling while one call waits for your
answer.

A configured local Home opens after local client unlock and verification by the
existing setup owner of its complete aggregate receipt, followed by matching
that receipt's operation and workspace request to the workspace receipt and
visible Home scope. A directory, running process, or worker is not setup proof.
The completed branch does not request the setup model catalogue or require a
fresh provider account observation; a browser setup draft does not supersede
the completed receipt. New and incomplete Homes still require the real provider
account and all setup proofs. An unavailable account observation is distinct
from a confirmed signed-out account and never becomes authenticated state.
Changing the selected Home discards pending client proof and account observations
from the previous target and validates the new target. This local shell admission
does not authorize provider requests: sending and provider-dependent operations
retain their runtime authentication, bindings, and formed grants.

The client preserves native publication-selection evidence in task authoring
profiles. Selected instruction artifacts remain independent bindings;
publication provenance never nests them as instruction text sources.
Task visibility uses the same sidecar contract as task creation.
Communication activity resolves recipient names and navigation from the exact
input interfaces on projected task records. Waiting for replies does not imply
that those tasks are executing. A live event cache is partial until a complete
history snapshot has loaded; opening a partial cache retains the loading state.

The desktop reads model descriptors independently of the task list and workspace
transaction lock. Task projections inspect workspace receipts without recovering
transactions; startup and the owning mutations perform recovery. A run changing
while its snapshot is read invalidates the next read, without indefinitely
restarting the current one. Registry and policy changes still require a fresh
configuration snapshot.

Task lists read the installed descriptors and run evidence without auditing the
package inventory. Package, application-catalogue, composer-capability and full-snapshot reads
load that inventory when they need it, retaining its current integrity checks.

The desktop client may persist derived task status and text previews under
`client/projection-cache/`. A status checkpoint includes the exact movement
prefix, ledger identity, reducer version, provider and execution scope, input
protocol, and the identities of every verified artifact file it consumed. A
matching checkpoint folds only appended movements. Previews are keyed by the
exact artifact and read contract. Changed source files or contracts invalidate
the result; malformed caches are discarded with a diagnostic. Cache write
failure never blocks an authoritative read. These files are disposable client
data: runtime admission and policy never read them, and removing them changes
only the cost of reconstructing the same result. Changes to the fold or preview
semantics require a new cache version.

Editing the latest idle operator message sends a new `operator.message` artifact
with `replaces_message` naming the exact admitted input. Its original attachments
and task bindings remain in place. The provider selects the conversation prefix
before that input, records `interface.context.updated` with `phase=message_revised`,
and starts a fresh private model context for the revised history in the same Run.
That revision participates in the provider's context identity and is replayed on
later turns and restarts. The client replaces the displayed turn only after the
provider receipt; older messages remain in the immutable ledger. This does not
undo tool effects. Concurrent work, an older input, or changed attachments refuses
the edit. Payload selection updates retained future tool routes together with
their templates, while pending effects and foreign-owned bindings remain exact.

Assistant display edits are client-owned documents at
`client-state/workspace/assistant-text-edits/<run-id>.json`. A version-1 document
contains `run_id` and `edits`; each edit names `provider_interface`, `turn_id`,
`item_id`, `original_text`, replacement `text`, and `edited_at_ms`. The client
matches the exact task, provider, turn, item and original text, applies the edit
after reading history, and retains it across duplicate deliveries. The answer is
marked Edited, with the original text available on hover. Removing the edit
restores the original display. These edits do not change model context, artifact
identity, tool evidence or the ledger. Ledger rows must never be rewritten to
edit a displayed answer.

Client authoring receipts prove completed file publications, not the continued
immutability of live runs or descriptors. Recovery checks the retained native
operation result and never republishes an applied write over later work.
Preimage checks protect new writes and rollback; read-only source observations
are not writes. Suggestions and task naming bind their admitted source artifacts
and leave the source run outside their write set. Shell admission reads the
workspace identity without replaying unrelated client transactions.

Client suggestions and task naming use a client-maintenance EE that is a
separate root beside the workspace's initial EE. Its template and worker ceiling
grant no tools or worker environments. The client projects projects and tasks
from its workspace subtree, so this service is outside that projection. The
owning package authors the root when preparing maintenance work and rejects an
existing maintenance EE with a different placement.

Task naming uses a separate client-maintenance run with private input, result,
worker and provider interfaces. The provider copy retains the task's selected
model, reasoning effort and service tier. Its request binds the exact admitted
user artifact and declares a text field, instructions and output schema; the
native provider generates from that input in an ephemeral context with no tools
or worker environment. The result crosses the private return interface as an
artifact. The client writes a valid title into the task's presentation only
while no authored name exists; a local manual alias also takes precedence.
Generation does not hold the task's execution lock. Completion or a bounded
failure removes the maintenance run's active bindings, and an uncertain native
request is never silently repeated. This operation currently requires a native
Codex binding; other provider bindings do not claim title-generation support.

Deleting a task includes its exact client-owned suggestion and title runs and
private ports under the same retirement proof as its other owned records. The client
checks each companion's source, topology and descriptors. Shared dependencies
still refuse deletion; ledger evidence and published shared memory remain.
The task's declared input intake and writer belong to this inventory even
without client presentation sidecars. Idle clients' declared output routes to
the task and exact policy allowances for its private resources are removed in
the same guarded transaction. A required wait, ambiguous owner or busy connected
client refuses deletion. Run-coordinate context artifacts remain readable
provenance; their recorded coordinates are not live bindings. Publication metadata
directories are checked as private storage and never treated as descriptors.
While native ownership is held, repeated retirement checks reuse only the
verified ledger prefix. Its bytes and every referenced grant remain checked;
new rows are decoded and cold recovery validates the complete prefix again.
Runtime-private directory contents are pinned after held quiescence, including
the runtime's own process-finalization writes. Their root identity stays fixed;
public descriptor trees must still match the original staging snapshot.

Context-free outgoing connections are future routing configuration. The provider
may recognize their exact addition or withdrawal between checked writer records;
withdrawing a connection requires a terminal handoff that does not use it.
Its checkpoint, frontier references and template are removed together, while
context, history, ownership and pending effects remain identical. Recovery still
proves every other edit through the declared writer chain, including consumption
of a completed tool result before the routing change. A routing edit never
substitutes for a writer receipt or grants a future emission.

![Applications catalog with a provider pack, installed connectors, and setup states](../images/client/applications.png)

[Drivers](ENVIRONMENTS.md#in-the-app) package worker-environment kinds.
[Client Extensions](../client.md#client-extensions) instead add widgets, tabs,
inputs, and guided repairs to the app. Use those when extending the presentation;
use the recipes below when extending what the runtime can do.

## What takes what

| You want | You add | Runtime code |
| --- | --- | --- |
| A model provider OpenCodex can serve | One entry in the OpenCodex bridge's `providers.json`; `scripts/sync-providers.ts` writes its folder and local package from the Codex template and the pinned OpenCodex registry; a provider served by the vendor's signed-in CLI declares `cli` (its commands, home paths and bridge environment) instead of a key | None |
| A provider over another existing wire | One interface folder: `interface.json` + `life.json` importing the wire owner's driver, with its own transport config | None |
| A provider over a new wire | Driver code inside the interface folder that owns that wire, then the folder above | None |
| A channel, sensor, or service | One interface folder, with a process if it must listen | None |
| A worker | A copied `system_worker_<name>/` folder: new id, edited ceiling | None |
| A goal | An `ee_template` folder | None |
| A tool over an existing room binary | A `tool.json` with its `handler_config` | None |
| A tool with new behavior | A binary in the owning kind's packages + `tool.json` | None |
| A room over an existing executor family | A `worker_environment_kind` folder + a door interface | None |
| A genuinely new kind of room | Kind-owned package code first, then the descriptors | None |
| A check | A `verifier` folder: descriptor + script | None |
| An outcome | A `mark` folder, then allow it in a worker's ceiling | None |
| A role, grounding text, or skill | An `artifact` folder, then bind it on a checkpoint | None |
| System data kept on screen | A feed interface folder without a life, a run that emits into it, and a client Extension that reads the feed | None |

The last column is the point.

## Where code lives

Every executable file belongs to a concrete owner folder:

- `registry/interface/<id>/packages/` - an interface's folder name is its
  routing id;
- `registry/worker_environment_kind/<name>/packages/` - a kind's folder name is
  the name of an execution-code type; nothing routes by it.

There is no neutral shared code home between owners. An owner reuses another
owner's code by importing it explicitly from the owning folder. The import
creates no shared identity, no family admission, and no policy surface.

What is never shared is the live executable: each kind runs its own door and
executor processes. Byte-for-byte clones of code per owner are not the model
either. Import is.

On macOS, the process sandbox permits read-only operating-system runtime
resources, including ICU locale data, alongside system libraries and timezone
data. Application files and credentials come from the process authority
declared by their owner.

A process selected through the developer-directory shim (`git`, `python3`)
runs with `xcrun_nocache=1` and exact read access to the selected developer
directory's SDK metadata, so the shim neither writes its cache nor reports SDK
errors under the sandbox. A workspace command's temporary directory is
`/private/tmp` when its sandbox may write there, otherwise its working
directory or another writable root, never `/`; a room that may write nowhere
runs its command with `TMPDIR`, `TMP` and `TEMP` unset.

The macOS desktop helper builds its pinned Swift package with SwiftPM sandboxing
disabled. This avoids nesting SwiftPM's manifest sandbox inside the caller's
existing process sandbox; the caller's declared filesystem, network and process
restrictions remain in force.

`read_executable_relative_paths` declares read-only runtime resources relative
to the parent of a resolved executable, after following its symlinks. Each entry
names an executable already in `executable_commands` and a relative path; a
missing path fails authority formation. For example, a Python verifier declares
`{"executable":"python3","relative_path":"../lib"}` for its installation's
standard library. The host resolves paths without knowing the language or
substituting a different interpreter. Copies cannot add these read grants.

## Add a provider

1. Copy the nearest provider folder that speaks your wire, for example
   `registry/interface/world_provider_ollama/`, to `world_provider_<name>/`.
2. Set `id` to the new folder name.
3. Edit `interface_driver_config`: base URL, model, request shape, and the
   *name* of the environment variable that holds the key. Never the key.
4. Keep `life.json` pointing at the wire owner's driver
   (`interface://<owner>/packages/...`), and list any environment names the
   process may inherit.
5. Add the new id to the `ceiling.interfaces` of the worker pins that may use
   it, and to any EE template that restricts `allowed_interfaces`.
6. Bind it on a run's checkpoints.

Nothing in the runtime changes, and nothing matches on the provider's name.

An admitted request that steers a native turn retains its exact input artifact
and emits `turn.control.acknowledged` with the new continuity. That receipt is a
declared context output, so the authored run advances with it before a later
tool result is consumed. A queue-only input receipt with no continuity remains
`turn.input.acknowledged` and does not claim a context change.

A tool, operation or operator result that settles one of several outstanding
calls of a native turn leaves the turn waiting for the rest. The provider then
emits `interface.context.updated` naming the `awaiting_results` with the new
continuity, so the run consumes that result and retires its result input before
the next one arrives; the last result resumes the model. A waiting result is
always consumed through its own admitted movement, in one write. Whatever checked
writer changed the run's context while the call waited — sibling calls and
results of the same turn, a steer, a control receipt, an earlier turn's
publication, an owner write — the consuming write records the context added
since the receiving checkpoint, in record order, as its rebase. Every checked
validator recomputes that rebase from the record before the write, and every
reader rebuilds the result's session context as its admitted context, then the
rebase, then the result. Instructions and role artifacts are not part of it; the
native turn refreshes them. Context that left the run while the call waited, or
a change without checked writer evidence, fails closed. A result is never
delivered twice.

The provider declares conversational input formats in `private_config.input_messages`.
Each label names the payload fields that carry its text, client observations,
attachments and request identity. An admitted message uses the same decoder when
the model is idle and when it is working; a follow-up reaches the current native
turn without waiting for its final answer. The original artifact remains the
reply reference through tool results and interruption. Labels and field mappings
belong to the provider package; the kernel continues to route by interface.

`text_fields` declares alternate text carriers; populated carriers must agree.
`client_context_field`, `attachments_field` and `identity_field` name their
payload fields, and `identity_required` declares whether the identity must be
present. `context_fields` names required accompanying data.
`request_context_fields` maps structured request fields to `array` or `object`;
when text is absent, nonempty declared request context carries the request. The
provider preserves the complete structured payload instead of selecting or
rewriting a transcript line.

An artifact from any other interface reaches the model as its structured
payload. When that payload carries `media`, the same entries a tool or operation
result uses — `mime_type`, the image's own artifact manifest and its bytes as
`payload_base64` — the model sees each PNG, JPEG or WebP image as an image, and
the payload text keeps the manifest and dimensions without the bytes. A
malformed or mislabelled image fails the input closed. This is how a connector
hands over the photos, screenshots and image attachments it received: the bytes
travel inside the one event artifact the kernel pins, so no task needs read
access to the connector's private files.

Native user-input requests cross the provider's declared question and answer
artifacts. The client renders the question and returns the selected or written
answer with its request identity; ordinary question-shaped prose creates no
request. The authored answer wait retains the exact request artifact and the
static context classification from the requesting turn, so responding after a
restart can recover the same native request. If the native thread reports that the
question's turn was interrupted by process death, the provider proves its exact
admitted question and verified answer, then resumes that thread through the existing
interrupted-turn continuation path with the answer in context. It does not submit a
reply to a dead RPC or replay an approval, elicitation or world effect. The operator
application package supplies the verifiers for its answer
and approval contracts. The bundled provider enables this native tool in default mode. Context
usage is a provider observation: the client uses the latest reported context size
and capacity, independently of cumulative billed token usage.

Capability amendments validate both the installed checkpoint frontier and the
future checkpoints declared by the provider's authoring topology. Candidate
formation includes the final descriptor updates and mandatory role bindings for
every reachable worker pin before any dependency or activation is published.
A future route cannot enter the live topology with an unbound mandatory role and
thereby invalidate the run's existing input or control checkpoints.

A model amends its own run while its turn waits on that amendment call. The
selection is planned against the frontier the turn will rearm, keeping the
retained templates' static context order, and rebased onto the current record:
the pending effect and its checkpoints stay exactly as admitted, and only the
template change and the new routes are applied.

Authored run previews and publication plans also validate the complete candidate
against each bound provider's declared authoring profile. They use the provider
owner's topology validator, including its frontier and future checkpoint
templates, before publishing any files. The model receives the validation
verdict, regressions and remaining issues without repeated registry snapshots.
This checks declarations and formation; it does not establish that a trigger
has fired or a result reached its consumer.

Intermediate provider observations are handed to their client route as soon as
they are complete. Their delivery does not close the source. The provider keeps
source completion on its final output or suspended external call, so reporting
progress neither waits for another model item nor releases a pending effect.
The client projects admitted message items immediately, including final-answer
items received before turn completion. A later sealed stream or terminal output
folds into the same provider, turn and item identity without repeating its text.

An indeterminate ordered write may have stopped before advancing its writer.
Recovery retains that failure, checks both exact file states, and never treats
the error as a successful receipt. A spent writer call needs an independently
authorized edit to its recorded next generation. A fresh attempt skips voided
generations; historical replay uses the next admitted preimage, not today's file.

Claude Code replay carries user attachments and tool screenshots as native image
blocks in conversation order. Whole-message history cuts remove their images
together with the message. Invalid image carriers fail before the CLI starts;
images are never replaced by captions or a text-only request. Native tool schemas
make types implied by `const` and `enum` explicit without coercing arguments or
rewriting literal data.

The native Codex/ChatGPT descriptors set `model_context_window=400000`.
Generated OpenCodex catalogues default `context_window` to the smaller of
the provider's `default_context_window` (400,000 when omitted) and the model's
declared capacity; a model named in setup, whose capacity is unknown, takes the
provider default. Claude Code and Anthropic declare 700,000 tokens;
`max_context_window` retains each model's capacity. The curated OpenCodex home
uses these per-model values without global context or compaction overrides.
A setup model offers the reasoning efforts its `providers.json` entry declares,
with `none` as the default; Custom offers `none` through `xhigh`, and `none`
sends no reasoning effort upstream.

The curated OpenCodex home keeps Codex's own retries at Codex's defaults. A
dropped response stream is retried up to five times (`stream_max_retries = 5`):
Codex samples the step again with the outputs of tools it already executed, so
an upstream disconnect does not end the turn. A Claude Code stream that ends
while a captured tool call is still open fails closed as `protocol_error`; the
bridge appends the content-free sequence of the CLI's last stream frames (frame,
block and stop types, never text or arguments), so the retried step's
`turn.error` names which frames left the call open. A request that fails before its
stream starts is retried up to four times (`request_max_retries = 4`). Every Codex-started descriptor also pins
`include_permissions_instructions=false`, because Codex's sandbox and
escalation instructions describe parameters Octopus tools do not have.
Every Codex thread Octopus starts carries an empty base prompt: Codex's own
product prompt never reaches the model, for native OpenAI models as for
OpenCodex ones, and the bound role in the developer instructions is the only
behavior text. A thread keeps the base it was started with when it resumes.

A task-private copy of an OpenCodex provider reads the setup values its
descriptor names (`auth_env_key`, `base_url_env`, `model_env`) from its
credential owner's private environment, through the owner root that composition
grants the copy; it is never handed copies of those values. A value also present
in the launch environment must match. A copy composed without that grant keeps
reading its launch environment.
Context usage remains the provider's reported usable
window; the client does not substitute a display-only limit.

The OpenCodex bridge pins its upstream release in `upstream.json`, with every
upstream surface it reads; the build fails when its sources read anything the
pin does not declare. Public destinations keep OpenCodex's 300-second stall
clock, and a local one (loopback, private, `.local`, `.lan`) uses the
interface's request timeout, so a silent local stream fails instead of
hanging. Before a request leaves the bridge, a tool schema whose root carries
`oneOf`, `anyOf` or `allOf` becomes a plain object schema with every branch's
fields, and the alternatives are stated in the tool's description; some
upstreams refuse root combinators, and the tool's owner still validates the
call against its declared schema. When an upstream answers with an error, the
error Codex receives carries the upstream's own reason when the body names one
(`error.metadata.raw`), not only the gateway's summary. A Chat Completions upstream's `reasoning_details` from an assistant message that called tools, its encrypted entries included, go back unchanged with that message in the next request, matched by the message's tool call ids, so a model that signs its reasoning keeps reasoning after each tool result. The managed Codex CLI pin lists every app-server notification Octopus
decodes and the ones it opts out of; installing a binary that sends any other
notification fails closed and names it. The provider descriptor also pins
which Codex features are on and which are off; a binary that reports a
different state for a feature the provider must prove fails the compatibility
check, and features that change what the model sees are listed as residual
sources. A Codex-family descriptor may declare
`private_config.retained_tool_output` with a tool `name` and the
`arrival_labels` that are Octopus-authored instructions; such an arrival starts
its turn as one retained, named tool output in the provider's tool namespace
instead of a user message, so the instruction survives compaction. Operator input
cannot be declared there, and a peer message that steers or is queued into an
active turn still enters as user input.

For the desktop picker, declare `private_config.model_catalog` in
`interface_driver_config`. Each entry names its `model`, `display_name`,
`default_reasoning_effort`, `supported_reasoning_efforts` and `show_in_picker`;
descriptions and service tiers are optional. The client validates these rows
and intersects them with the interface's model allowance. It does not inspect
the provider's transport adapter.

For account-backed discovery, declare `private_config.client_metadata` with
`schema_version: 1` and `module`, an exact `interface://` import of a CommonJS
file under the owning interface's `packages/`. That file exports
`schemaVersion: 1` and `modelCatalog`, and optionally `probeAccount` and `signIn`
for desktop setup. Discovery receives the descriptor, desktop account transport
and request-local cache and returns the client's model rows. Keep account
validation in the owner module. Missing or failing modules produce an error;
the client never replaces their result with another account or a static list.
Ship the module and its declaration in the same package update.
The installed package owner must declare that exact module. Execution loads
that regular file and its imports from the active registry, and validates the
exported API and returned catalogue. Authorized local code edits do not need
to match a bundled copy. Before installation, setup discovery uses the selected
source registry; it never substitutes source code for a missing installed module.

A signed-in CLI provider owns its authentication and declares the executable
and account paths its life may use, including subprocesses used for account
access. Claude Code on macOS requires both `sh` and `security` to read Keychain.
The bridge supplies its interface-private
temporary directory to the CLI explicitly; the client does not copy tokens or
choose a replacement provider.

Grok CLI runs the signed-in `grok` headless for one model turn per request. Its
home is a turn-private directory holding only a capture catalog of the client's
tools, so Claude Code skills, plugins and MCP servers on the Mac stay out; the
CLI's own sign-in stays in `~/.grok`. The turn's system prompt is a private
agent profile that lists the catalog, the CLI's built-in tools and tool search
are removed, and `--max-turns 1` ends the CLI after the model's first message.
Grok reaches catalog tools through `use_tool`; those calls, parallel ones in one
message, return to the client as its function calls. The capture server
acknowledges a call so the CLI can close that message, and no model reads the
acknowledgement. A call outside the catalog fails the turn. Grok CLI turns carry
text only.

A replayed intermediate output does not establish that earlier context has been
restored. Snapshot recovery proves inserted history entries through the checked
source and output publications, preserving their content and order.

A retained output whose original edges are still pending remains the same
hand-in. The provider captures its artifact once and waits within its declared
result timeout, retrying only that retained delivery identity and scope. Pending
admission does not end the native turn. Cancellation, an expired timeout, a
changed scope or a definitive refusal remains an explicit failure; completed
edges are never executed again.

A completed source continues only explicitly. When a provider owes further
outputs to an arrival whose terminal batch is already handed and fully admitted,
it reopens that source and hands another terminal batch that declares
`continuation_of`, the last output of the batch it continues; continuations
chain, and no new arrival or input consumption is recorded. The batch carries
only declared calls and client projections of the same contract — no session
snapshot, operator request, outgoing binding or capability amendment — and adds
only the result waits of its own calls; its context outputs join the
predecessor's publication and advance the context generation once, exactly as an
ordinary batch of the same outputs would, while the live context stays
untouched. A second terminal batch for a completed source without this declaration
is still refused by checked replay and input revision.

A terminal pending provider handoff with exact refusal evidence for every
artifact is a failed source, not an unresolved native effect. Recovery uses the
same refusal proof as failed-source settlement; an admitted edge, an unconfirmed
outcome or a temporary admission refusal cannot take this path. The provider
publishes the failure through its declared route and continues processing
operator controls without repeating the rejected operation.

A final client projection that failed before hand-in may also close as a failed
source. Its durable dispatch fence, absent hand-in identity and marker, and
complete ledger prefix must prove that none of its final artifacts reached an
edge. Already delivered progress remains admitted history. Recovery additionally
requires an admitted terminal `turn.error` for that exact source and pinned run
record before it accepts a later source. It reconstructs the original input and
completed tool results from checked writer and ledger evidence, without admitting
the undelivered final output or repeating an effect. Native requests and uncertain
deliveries remain unresolved.

Session snapshots retain their immutable parent prefix when ledger validation
fills previously absent movement coordinates. Prefix identity includes labels,
payloads and exact artifact references; every movement coordinate is validated
separately. Appending recovered history does not force another full-history copy.

A deterministic projection package may explicitly replace a final artifact that
its verifier rejected. The checked writer proves the exact predecessor record,
pinned source, retained delivery scope and definitive refusals for every original
edge. Any admitted, pending or ambiguous edge prevents replacement. The successor
keeps the output contract and neighboring run state, changes the artifact and
checkpoint generation, and retains the rejection proof. Replaying its write uses
the same saved successor; unchanged output bytes do not create another delivery.
Old artifacts, movements and grants remain unchanged. Model providers do not
select this projection-repair operation.

A retained result-invocation write is validated against its exact authored
preimage, patch and postimage. If another interface has advanced the record,
retry uses the invocation owner's assembler over that current record and proves
the same admitted source, receiving checkpoint and original contract. The
concurrent-edit checks still protect unrelated context, pending work and peer
state; a later record never substitutes for an earlier attempt's preimage.

A provider handoff reads its run through the record it last proved. Execution
is concurrent: when another declared writer advances the record while the
handoff observes it, the observation is not a verdict. The handoff adopts the
advance through the checked writer chain and observes again within its result
timeout; a waiting result's rebase grows by the context that advance added. An
advance without checked writer evidence, or a record still moving at the
timeout, fails closed with the original error.

The result wait binds its exact original call as read-only evidence. Provider
continuity reads that call independently of model-context filtering. An older
conversation context may be superseded only by the newer pending state in
that admitted call, with matching provider, run and transport coordinates.
Completing an older result can project the same generation as a newly admitted
call. That projection cannot clear the new call's wait: its exact call evidence
prevails until an equally new recorded continuity carrier proves a successor.

A native turn may wait on several external calls at once. The provider admits
every ordinary dynamic tool or interface-operation request that arrives while
earlier calls wait, emits each as its own call artifact and records the whole
set in its continuity, so every snapshot names exactly the calls still
outstanding. A request that arrives after the turn's drain suspended wakes the
provider: it is drained through the same path, after that turn's notifications
buffered before it, and what the drain emits continues the completed handoff of
the turn's latest arrival in terminal batches of that source movement; no new
arrival is recorded. The latest arrival is the input or result whose drain
suspended, or a later steer, queued message or duplicate result whose receipt
completed that source. The request instead waits for the next arrival while a
control applied to the waiting turn has no handed receipt, while a movement the
turn never saw owns the run frontier, or when a provider error notification
precedes it. A continuation that run authoring refuses fails closed.
Results settle their own call in any order. When the native turn has already
ended while several of its calls are outstanding, whether the app-server
completed or aborted it or the provider process died, each late result except
the last settles only its own call: its closure carries the exact terminal
proof, records that the result never returned to the model, and leaves the
remaining calls outstanding. The last result settles exactly as a single call
does: a completed or failed turn closes with its retained native outputs, and a
turn cut by process death continues in a new turn whose context carries every
late result. A native turn still running on a restarted provider with several
lost requests fails closed. Native run operations, operator requests and
lifecycle-held calls are admitted one at a time and wait for the next arrival,
and an operator hold belongs to one outstanding call. After a Stop each
outstanding call closes with its own interrupted result and only the last
observes the native terminal; a request issued before the interrupt was
acknowledged stays buffered and is quarantined by that last drain. Changed tools
or instructions refresh a native turn only once a single call remains
outstanding.

The provider's explicit `reseed_from_session_context` cold-start policy rebuilds
its private transport from the admitted session snapshot. It requires that
snapshot and a non-regressing context generation. A pending or interrupted
result forces exact thread resumption instead; changing this configuration does
not settle a tool call. The new thread receives the ordered admitted context and
current formed policy, while the original native history remains untouched.
This mode makes no claim that old native wrappers completed or that code after
an await ran. When recovering damaged native history, that uncertainty belongs
in the admitted continuation input and remains part of the session context.

A provider context transition retains the exact terminal proof for its retired
native turn. A late request with those same thread and turn coordinates is
recorded and explicitly rejected without dispatching an effect; it cannot be
executed under the replacement context. Requests without that exact proof still
fail closed. Terminal error and control receipts count ledger-admitted standing
context generations as well as the last authored record. A protocol failure
closes its private transport before returning control, so an abandoned model
turn cannot keep issuing requests after its task has failed. A source that fails closed for any
other reason retires its native turn the same way before its `turn.error` is
handed, so later results of that turn reach no live model turn. A terminal error
that cannot be handed closes its source through the declared failure route
instead of ending the provider life. A failed source with neither a failed-source
handoff nor a proven standing route is recorded as the interface's admitted
failure of that movement, and the life keeps serving its other sources. A
stream sealed with another terminal, or for another source, is not the terminal
a recovery looks for; an unsealed terminal boundary still fails. A source whose stream was already sealed with
its `turn.error` is never dispatched again: on restart the provider hands that
exact retained error, or closes the source through the failure route when it
cannot be handed.

An explicit Resume on a context-free control route recovers its native thread
from the provider's delivered stopping receipt. Its run, provider and context
generation must match the current record, and its exact acknowledgement must
have entered the ledger before Resume. The control route does not need to
carry the conversation or create a replacement thread.

## Add a channel or sensor

1. Create `registry/interface/world_<function>/interface.json`. Name it for
   what it *is*, not for a brand convenience.
2. If it must listen, add `life.json` and the process. Any language: it reads
   its movements from the ledger or the SQLite index and hands artifacts back
   as JSON lines.
3. Decide what counts as heard. The interface is the threshold of
   artifact-space; filtering raw input is its job, not the kernel's.
4. Bind it on a run's checkpoint. Until something binds it, it does not run.

Start from `registry/interface/world_pyecho/` - a complete Python interface in
one file - and [registry/interface/GUIDE.md](../../registry/interface/GUIDE.md).

## Add a worker

1. Copy a `system_worker_<name>/` folder.
2. Change the id in both JSON files.
3. Edit the ceiling: the interfaces it may reach (providers, doors, other
   workers), its tools, its room kinds, the marks it may close.
4. List its `role_artifacts`, creating new artifact folders for the role text.
5. Allow the new pin wherever an EE template restricts `allowed_interfaces`.

Two JSON files. A tool that lives in a room must be reachable through one of
the pin's room kinds, and every mark id must exist, or loading fails closed.

## Add a tool

1. Find the room it belongs to. A tool belongs to exactly one kind.
2. If an existing binary of that kind can do it, write
   `registry/tool/<id>/tool.json` with a `handler_config` selecting the
   operation.
3. If not, add the binary to the kind's packages and declare it among the
   kind's owned tool binaries.
4. Declare only the authority the process needs. For a path that arrives in
   arguments, declare it as an argument path within specific root slots rather
   than granting the room.
5. Add the tool id to the ceilings of the workers that may hold it.

See [registry/tool/GUIDE.md](../../registry/tool/GUIDE.md).

## Add a check and an outcome

1. `registry/mark/<id>/mark.json` - the vocabulary entry.
2. `registry/verifier/<id>/` - `verifier.json` and a script that reads the
   request on stdin and prints `{"passed": …, "details": …}`.
3. Allow the mark in `ceiling.mark_outcomes` of the workers whose checkpoints
   may close it.
4. Reference both from a checkpoint's `required_verified_artifacts`.

Do not substitute verifiers for marks or the other way around. A blank check is
the explicit `artifact_exists` verifier, never a hidden pass.

See [registry/verifier/GUIDE.md](../../registry/verifier/GUIDE.md) and
[registry/mark/GUIDE.md](../../registry/mark/GUIDE.md).

## Add a room

1. If an existing executor family fits - process execution, browser, desktop,
   container, MCP - write a `worker_environment_kind` descriptor over it and a
   door interface whose driver belongs to that kind.
2. Declare the boundary ceiling in `boundary_defaults`. Policy can narrow it
   and nothing can widen it.
3. Declare new symbolic root slots, and bind them once in `host.json`.
4. If no family fits, write the executor as kind-owned package code first.

See [registry/worker_environment_kind/GUIDE.md](../../registry/worker_environment_kind/GUIDE.md).

## Time and inactivity triggers

A trigger is an interface folder with its own condition: when the condition
holds it sends an artifact, and every run whose checkpoint pins the folder
receives it. A requirement-free checkpoint is a standing route, so one binding
receives every firing and nothing re-authors the run. A time trigger is a copy
of the clock interface `world_cron`: its descriptor declares `copy_of`, and its
activation holds only the schedule — an interval, a cron expression, a due time
or a quiet period — after which it sends `cron.current_time`. The interface never
edits a run and never creates one.

The clock interface supports inactivity: selected runs' artifact movements
restart a declared quiet period, after which one artifact wakes the runs that
pin the trigger. Selection is a descriptor match, optionally narrowed to one EE
subtree; a second match excludes runs bound to or fed by matching interfaces,
which is how a time trigger declares that other triggers' runs are not the
user's work. Runs that bind this trigger or received its artifact are excluded,
a due period fires exactly once, and admission evidence survives restart.
Selection, timing and cursor state belong to the interface package; the client
projects these activations as Triggers, with the tasks that pin each one. A
disabled time folder that no run binds is a template, shipped by a package.
An Application Pack's `workspace_setup.triggers` names templates that setup
arms only when its package was selected: setup copies the template folder under
its own interface id and quiet period, then creates the entry's `task` — worker,
context artifacts, rooms and door slots, with the provider and model chosen for
Home — whose standing input pins that folder. A retried setup finds the armed
trigger and its task and creates nothing. Deleting a trigger deletes its folder;
the tasks that pinned it remain ordinary tasks.

A review task that a trigger wakes keeps one change journal per firing, under
`state/change-journals/<run>/<now_unix_ms of the firing>/journal.json`, so
Triggers lists each review with its changes and undoes one while the files still
hold what it wrote.

Triggers also lists World event sources with active inbound run bindings: sensor
interfaces, and applications whose `private_config.task_input_labels` declares
what a connected task receives unprompted, such as incoming mail or messages.
Their receiving tasks and admitted deliveries come from run records and the
ledger; an application's deliveries count only its declared task inputs, not its
operation results. An active binding is
not evidence that an external event has arrived. Event sources expose no cron
edit, pause or delete controls; their configuration belongs to their package.

Operator-opened artifacts from the tool trace, Sources and Outputs use the
existing right Files pane in their originating client workspace. A proven read
retains its exact admitted content or excerpt and its run/artifact coordinates
as a bounded, read-only client preview. The Files pane preserves its ordinary
draft ownership and Save/Discard/Cancel path. Opening this preview creates no
run, publication or model authority.

Home uses compact widgets as its ordinary visual presentation, including during
voice calls. Longer results may span several focused widgets; long text belongs
in a saved document. The correlated `open_file` client control reads a text file
through the existing admitted Files roots and requests its tab in the right
pane. Its result identifies the readable target and opening request, not rendered
pixels. Missing, unreadable, binary and oversized files fail explicitly. It
executes no file and starts no external application.

Home binds a compact client-delivery artifact separately from its full authoring
coordinates. The compact delivery contract is included in the provider's
declared instruction selection. Client controls use it directly; configuration
and source edits read the separate coordinates. The client tool declares the common widget call shape directly so
these operations need no full document-authoring catalogue.

Text and voice inputs can carry a compact `client_observation` snapshot of the
available surfaces and widgets, including exact identities, revisions and current
presentation, configuration and review states. The snapshot travels inside the
input artifact as read-only client context. It grants no authority; actions still
validate the current revision and route. Missing or stale details are read through
the client tool. The client bounds the snapshot to 4 KiB by omitting whole rows and
reporting the omission counts. During a voice call, a changed snapshot crosses the
declared control interface as delegation metadata; subsequent delegations carry
the latest snapshot accepted by the voice provider. Replaying an input keeps its
original snapshot.

An in-progress projection refresh retains its last admitted observation; it does
not report an unavailable inventory. A failed read does. Client observations stay
in the voice interface's private state and enter the execution model only inside
its delegation artifact. They never enter the audio model's conversation or
change its session configuration, including at call creation. A metadata update
has `client_context`, `channel=context`, no delegation identity and no text.
The interface persists the snapshot and its `context_retained` receipt together;
that receipt makes no claim about a transport write or spoken response. Replaying
an older accepted update returns its receipt without replacing newer context.

Client controls are ordinary system-room tool calls. An operation declared with
`await_client_result` emits an exact `octopus.client_action_request` artifact
through the presentation interface. The client retains preparation and its
actual result by request identity, then sends `client.action.result` through
its declared input. A standing route delivers it to the system room, where
it completes the original native tool call. It never starts another model turn.
The result must match the source run, tool-call artifact, request artifact and
request id. Restart reuses retained emission and result evidence; an admitted completion
also prevents replay after the client loses its local cursor. An uncertain
effect is never repeated automatically. A missing client produces an explicit
bounded timeout. A stopped or expired request performs no new client action.
Presentation requests themselves do not echo back into the provider.
Projection updates wake the client control consumer without cancelling an
admitted read, effect or result delivery. Refreshing an already loaded surface
registry keeps controls available; changing Home or disabling the client cancels
the old consumer. Repeated wakeups coalesce while one consumption is in flight.

The voice provider delegates request text to Home; it does not implement native
widget function calls. Home uses the same client control tools and streams its public output. Provider continuity retains the exact original input artifact
across tool results and interrupted-turn recovery; projected outputs carry that
reference as `in_reply_to`. The client joins it only to an admitted input from
Home's declared voice interface and returns the result to that exact native
delegation in the same call. Results from earlier calls are not appended to the current call;
arrival order never assigns a result to a different request. Prior Home messages enter a call as an explicit historical
snapshot, not synthetic native voice turns without their tool evidence.
That snapshot includes requests from Home's exact voice delegation interface;
live microphone input is not echoed back into the call as a text update.
Public progress reaches the voice commentary channel; answer text and failures
reach the speakable channel as they become available. The client preserves each
item's identity and sends only its unseen suffix. Completing an item never
replays its prefix, and changing an already delivered prefix fails explicitly.
Transport chunks carry the model's text, channel and exact delegation identity,
without client JSON wrappers, extra instructions or synthetic completion messages.
Call creation returns its SDP before the control connection is established,
allowing WebRTC to activate the remote session. The provider joins that exact
call with bounded activation retries. The microphone remains disabled until
both WebRTC and an admitted provider-ready event are confirmed. An authenticated
sideband join publishes attached state for that exact call; joining an existing
call does not wait for its earlier session-start event.

## Media across interfaces

The voice provider declares conversation, interruption and delegation policy in
its session instructions. GPT-Live instructions name the native
`<|SpawnThinking|>` signal explicitly; only an admitted delegation, followed by
backend result artifacts, proves execution. A spoken acknowledgment does not.

Carry files as artifact references, not filesystem path strings. A transport
adapter may materialize bytes however it needs; the framework surface stays
artifact-native.

## Who may change the registry

Authority is policy, and the mechanism is ordinary file tools. A run whose room
covers a registry path can write there; the write is a gated, recorded tool
call. A narrower run asks by emitting an artifact to a run that already holds
the authority.

## Before adding code

Ask:

- Is there a registry or descriptor layer where this behavior belongs instead
  of a branch?
- Does this add a branch on a worker's name, a provider's name, a model string,
  or an artifact label? Then stop.
- Does every new side effect leave an artifact and a movement?
- Is failing closed with a clear error better than a fallback that almost
  works? It usually is.

### Native video documents

A native `video` node accepts an explicit HTTPS media `source` without URL credentials and an optional `title`. It uses the platform video controls, anonymous cross-origin loading and metadata-only preload; it never autoplays. The media server must permit CORS. Publishing a new document revision replaces the source. Load failures remain visible. This media capability does not grant sandbox HTML network, iframe or media access.
