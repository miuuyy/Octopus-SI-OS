# Extending Octopus

The cookbook for adding capabilities without editing the runtime.

## It should work like a package

Adding an interface, a worker, a tool, a verifier or a room should feel like
dropping in a portable bundle and editing a few JSON bindings. "Package" is a
distribution layout, not a runtime primitive: once installed, the runtime sees
ordinary descriptors, exact interface folders, bindings, artifacts, policy,
marks, rooms and runs.

For an existing family, a new channel, provider or worker preset is descriptor
and binding data. If adding one channel requires editing app bootstrap, session
logic, or a manager named after the concept, a primitive has leaked. A genuinely
new execution family is added once as code inside its owner folder; every later
instance of it is data. Handler and verifier refs are global inside one
composition root, and duplicates fail closed instead of letting load order pick
a winner.

Installed owners may carry authorized local edits to descriptors, role artifacts
and code. Health checks validate identities, structure, declared imports and
probes, not byte equality with a distribution.

Octopus is a research preview. An incompatible persisted format needs a fresh
Home through normal setup: there are no migrations, automatic package adoption
or startup repairs of old state. Keep sign-in credentials when resetting a
disposable Home; never transplant old runs, checkpoints or ledgers.

## What takes what

| You want | You add | Runtime code |
| --- | --- | --- |
| A model provider OpenCodex can serve | One entry in the OpenCodex bridge's `providers.json`, then `scripts/sync-providers.ts` | None |
| A provider over another existing wire | One interface folder whose `life.json` imports the wire owner's driver | None |
| A provider over a new wire | Driver code inside the interface folder that owns the wire, then the folder above | None |
| A channel, sensor or service | One interface folder, with a process if it must listen | None |
| A worker | A copied `system_worker_<name>/` folder: new id, edited ceiling | None |
| A goal | An `ee_template` folder | None |
| A tool over an existing room binary | A `tool.json` with its `handler_config` | None |
| A tool with new behavior | A binary in the owning kind's packages + `tool.json` | None |
| A room over an existing executor family | A `worker_environment_kind` folder + a door interface | None |
| A genuinely new kind of room | Kind-owned package code first, then the descriptors | None |
| A check | A `verifier` folder: descriptor + script | None |
| An outcome | A `mark` folder, then allow it in a worker's ceiling | None |
| A role, grounding text or skill | An `artifact` folder, then bind it on a checkpoint | None |
| A trigger | A copy of a clock or sensor interface with its own condition | None |
| System data kept on screen | A feed interface without a life, a run that emits into it, and a client Extension that reads the feed | None |

The last column is the point.

## Where code lives

Every executable file belongs to a concrete owner folder:

- `registry/interface/<id>/packages/` - an interface's folder name is its
  routing id;
- `registry/worker_environment_kind/<name>/packages/` - a kind's folder name is
  the name of an execution-code type; nothing routes by it.

There is no neutral shared code home. An owner reuses another owner's code by
importing it from the owning folder, `interface://<owner>/packages/...`; the
import creates no shared identity and no policy surface, and a missing import
keeps the importing interface in coma. A live executable is never shared: each
kind runs its own door and executor processes. Helper executables imported from
a kind are granted as exact `executable_root_subpaths`, never as the whole kind.

Secrets are not registry data. Each interface owns its env files, keys and
sign-in inside its own world; descriptors name the environment variable, never
the value, and a run sees a handle or a path, never the secret.

## Interface life

An interface lives by itself. Its `life.json` declares the processes it runs:
a `bin` (its own build output or an `interface://` import), arguments, I/O
limits and an explicit authority over root slots, paths, executables and
environment. Only the declared authority exists; for a resource next to a
resolved executable, such as an interpreter's standard library, declare
`read_executable_relative_paths` (`{"executable":"python3","relative_path":"../lib"}`).

- An installed interface that no run binds does not start.
- Malformed life or activation data fails closed for that interface only;
  configuration and binding edits are picked up without restarting the app.
- Stopping a life stops its whole process group. Each start keeps a bounded
  stderr capture in the interface's private directory (the sixteen newest).
- A life with nothing to do prints `{"kind":"asleep","through_seq":N}`, naming
  the first movement it has not read, and exits 0. That is sleep, not failure:
  a movement at or past `N` lifts it again. A life that serves a socket may name
  it in the frame; the runtime holds the socket while it sleeps and the first
  byte of a caller wakes it with the caller still connected. World-listening
  lives never sleep.

A life can declare `private_environment_root_slot` to read its declared
private environment names from an explicitly bound owner in the same Home.
Each process must hold read authority over that slot. Its own values and the
owner's values must agree wherever both are present. Owner changes participate
in the life fingerprint. Provider copies bind this slot to their configured
credential owner, including Vertex project and credentials-file settings.

An interface that listens for external arrivals adds `activation.json`:

```json
{
  "schema_version": 1,
  "interface": "<id matching registry/interface/<id>>",
  "activations": [
    { "activation_driver_ref": "app_server_transport", "config": {} }
  ]
}
```

Every admitted raw item is recorded as an arrival before any model is driven
from it.

## Delivery is a ledger movement

Transfer goes through the runtime on both legs: the grant, the movement in the
ledger, the verifier on the edge. An interface hands its artifact to the runtime;
on the return leg the runtime records each outbound transfer as a movement and
the receiving interface reads its own movements from its cursor. There is no
folder bus, queue or lease: an unread result is not a loss. Because only an
artifact crosses the boundary, a driver's language is invisible to the kernel; a
Python or JavaScript interface hands in artifact files and reads the JSONL
ledger or the `ledger-index.sqlite` cache.

## Add a provider

1. Copy the nearest provider folder that speaks your wire to
   `world_provider_<name>/` and set `id` to the folder name.
2. Edit `interface_driver_config`: base URL, model, request shape and the
   *name* of the environment variable that holds the key. Never the key.
3. Keep `life.json` pointing at the wire owner's driver and list the
   environment names the process may inherit.
4. Add the id to the `ceiling.interfaces` of the worker pins that may use it,
   and to any EE template that restricts `allowed_interfaces`.
5. Bind it on a run's checkpoints.

Nothing in the runtime changes, and nothing matches on the provider's name. A
provider is a driver: it receives the request artifact and emits each outcome
into the binding that carries it, a reply into the outbound binding and every
tool call as its own artifact through the gate. It owns its context assembly:
the grant rides the invocation and the provider renders it into the prompt and
the tool list. Its native state is recovered from the run record and the ledger,
never from memory, and a turn it cannot finish ends as `turn.error` on the run's
declared route. An output larger than the host's `max_payload_bytes` fails
closed with both sizes.

### Providers served through OpenCodex

Most providers are one entry in the Codex provider's OpenCodex bridge,
`registry/interface/world_provider_codex/packages/octopus-provider-codex/opencodex-bridge/providers.json`.
`scripts/sync-providers.ts` writes each provider folder and local package from
the Codex provider template and the pinned OpenCodex registry, so every provider
moves to a new OpenCodex release together; `--check` fails when a folder drifts
from the list. An entry authenticates in exactly one way:

- `key` - an API key whose environment name the Applications setup asks for;
- `local` - a local server, no key;
- `cli` - the vendor's own signed-in CLI (Claude Code, Grok). The bridge runs
  the CLI for one model turn per request in a turn-private home that holds only
  a capture catalog of the client's tools; the CLI keeps its sign-in, its
  built-in tools are off, and the life is granted exactly the declared commands,
  home paths and environment names;
- `account` - a sign-in performed by the Application's connection module through
  the bridge's account command (see Antigravity below);
- `vertex` - a Google Cloud project and a credentials file, billed to that
  project.

A provider may declare a static catalog together with an environment-backed
model alias. Selecting a catalog model uses its own upstream id. Selecting the
alias requires its configured id.

An explicit `models` list supplies the catalog the model picker shows; keep
model additions there and regenerate. Context windows default to 400,000 tokens,
700,000 for Claude, never above a model's declared capacity.

The bridge's `upstream.json` pins the OpenCodex release and lists, as
`path#symbols`, every upstream surface the bridge reads; the build fails when the
sources disagree with that list. With `preserveCallerContext`, OpenCodex sends
the caller's instructions and tool descriptions as written. The managed Codex
CLI each provider starts carries an empty base prompt and
`include_permissions_instructions=false`: the bound role is the only behavior
text. What Codex adds that cannot be turned off is listed in the descriptor's
`residual_ambient.residual_sources`. An answer no retry can change (401, 402,
403, 404, a 429 with exhausted quota) ends the turn with the provider's reason;
the bridge reports exhausted quota separately from temporary rate limits and carries
provider-supplied retry timing. Codex retries transient failures within the descriptor's
`private_config.opencodex.native_stream_max_retries` (default five). The Claude
CLI descriptor declares zero: its request deadline ends the turn, without
replaying the CLI through native stream retries. The CLI and bridge share the
declared `transport.request_timeout_ms`; expiry returns the deadline in the error.
The bridge serves no hosted web search:
runs search through Octopus's own `web.search` and `web.extract`.

Google's adapter accepts `private_config.opencodex.options.google_safety_settings`
as the native array of `category` and `threshold` pairs, sent as `safetySettings`
on every request. The AI Studio provider declares `OFF` for all five supported
categories, the minimum configurable filtering. Google's built-in protections
and the model's own refusals remain. A blocked prompt or response retains
Google's reason and safety ratings in the terminal error.

### Antigravity

Antigravity runs on Google's official Antigravity ACP server, declared by the
`acp` block of its `providers.json` entry; `antigravity-acp.json` pins the
release per platform. Connect installs the verified server into the owner's
`opencodex-account/` and signs in through the server itself, which keeps the
token as a private file there. A provider maps each Codex thread to its own ACP
session. The session instructions carry the run instructions and the full MCP
catalog, including each granted function's name, description and JSON input
schema. Google's native `call_mcp_tool` dispatches the chosen function to the
private MCP endpoint. The provider checks its server and function name against
the current grant. Codex executes the call through its declared route.
The admitted result answers that same MCP request. Large text travels as a
`text/plain` document, including inside a function result.

The bridge accounts for both the current ACP trajectory and the caller history
required to reconstruct it. The context count uses the greater of the current
exchange's `usage_update.used` and a UTF-8 bytes/4 estimate of replay text,
including instructions, tool schemas, text attachments and output. Images,
audio and PDFs use the native counter; their base64 transport encoding is not
text input. The replay estimate does not measure their token cost. Responses
usage exposes `context_estimated`, `native_context_tokens`,
`replay_text_estimated_tokens` and `replay_unmeasured_media` so the estimate's
provenance and coverage remain visible. This lets the caller compact retained
text even when ACP has already compacted its own trajectory. The ACP prompt
total can span several model calls.
Its breakdown is included only when it agrees with a tool-free exchange's
occupancy. An SDK-confirmed input overflow becomes `context_length_exceeded`
so the caller can enter its native compaction path.

After a completed turn the process exits. The next input resumes the session
when the caller's history extends the recorded response. Replacing history
starts a fresh ACP session with the supplied transcript. If a restart interrupts
a pending MCP call, ACP cannot reconnect that call. The provider restores the
admitted transcript with its call ID and result in a fresh ACP session. The
same recovery lets the bridge release an idle connection after its configured
transport timeout while Octopus continues waiting for the tool. A retry
of a recorded request returns its recorded response and call ID.
Replayed assistant content is ordered typed JSON, including historical calls
and their exact arguments, rather than prose that resembles a new tool call.
Only an actual request to the declared MCP endpoint issues a tool call.
Live ACP updates for the active session count as upstream progress even when
they contain no text. These heartbeats are not retained as conversation content
or replayed from the response cache; silence still reaches the stream deadline.

The descriptor lists the server's residual instructions and MCP resource tools.
The client endpoint publishes only its declared functions. A native execution
failure reaches Codex as a failed response, including when ACP formats it as
an agent message.

### Model catalogs

The desktop model picker reads `private_config.model_catalog`, intersected with
the interface's allowed models. A provider whose models depend on the account
declares `client_metadata`: an `interface://` import of a CommonJS module in its
own `packages/` that exports `modelCatalog` (and optionally `probeAccount` and
`signIn` for setup). A failing module is an error, never a static substitute.

### Checked run revision

The provider's `preview_revision` and `revise_run` accept either exact file
writes or `authored.run_patch`: the target's `file.read` content hash and RFC
6902 operations. `file.read` with `json_pointers` returns only selected JSON
fields and the complete file's hash from the same read, so inspecting a field
does not load the complete record into context. The patch applies only to that
exact existing run; stale
hashes, a second full write of the same target and edits to the in-flight
caller are refused. Dependency writes precede the patched run. Both forms
validate the complete candidate, preserve accepted work and publish through
ordinary granted file writes with exact readback. When creation selects tools by
environment kind, the declared door roots follow the selected kinds. An explicit
map supplies the exact root overrides.

Peer previews return a summary and file counts. Small proof lists and validation
reports fit in the default response. To inspect any part of the exact proposal,
repeat the preview with the same creation key and `preview_read.pointer` under
`/authored`, `/run_contract`, `/files` or `/validation`. Each page contains
up to 4096 bytes of JSON text. Continue with its next byte offset and content
hash. A change to the selected value invalidates the continuation.
Current-run previews return a future-contract hash, counts and write proofs;
successful amendments return the checked readback hash, counts and stage
receipts. The readback validates the full record without returning its private
interface state.

Explicitly selected Octopus `web.search` and `web.extract` remain visible on
Codex and ChatGPT as well as the bridge providers. Codex's hosted search is
independent of those granted tools. The web driver defaults to the configured `custom` endpoint. Select
`keenable` in a tool call to use the bundled keyless endpoint.
`web.extract` with `include_raw_content: false` returns only normalized content.

## Add a channel or sensor

1. Create `registry/interface/world_<function>/interface.json`. Name it for
   what it *is*, not for a brand convenience.
2. If it must listen, add `life.json` and the process. Any language: it reads
   its movements from the ledger or the SQLite index and hands artifacts back.
3. Decide what counts as heard. The interface is the threshold of artifact
   space; filtering raw input is its job, not the kernel's.
4. Bind it on a run's checkpoint. Until something binds it, it does not run.

What a connector can do (post, reply, edit, react, upload, read a thread) is its
operation catalog: one JSON file per action under `operations/` with its input
schema, description, request label and one result label. The model calls each
action as its own tool; the connector answers each request exactly once, and an
action whose outcome is unknown after a restart is reported as
`indeterminate_external_effect`, never repeated. What a connected task receives
unprompted, such as incoming messages, is listed in
`private_config.task_input_labels`. Images arrive inside the event artifact as
`media`, so the model sees them as images.

Start from `world_pyecho/`, a complete Python interface in one file, and the
interface guide `registry/interface/GUIDE.md`.

## Triggers

A trigger is an interface folder with its own condition: when the condition
holds it sends an artifact, and every run whose checkpoint pins the folder
receives it. A requirement-free checkpoint is a standing route, so one binding
receives every firing and nothing re-authors the run. The interface never edits
or creates a run.

A time trigger is a copy of the clock interface `world_cron`: its descriptor
declares `copy_of`, and its activation holds only the schedule (an interval, a
cron expression, a due time or a quiet period), after which it sends
`cron.current_time`. A firing due while a pinning run is working is sent once
when that run is idle; missed ticks are not replayed. An inactivity trigger
restarts its quiet period on the artifact movements of the runs it selects by
descriptor match, optionally within one EE subtree, one exact EE or a named set of runs, and can filter activity by artifact name and direction. It fires once per quiet
period. A disabled clock folder that no run binds is a template; an Application
Pack's `workspace_setup.triggers` arms templates at setup together with the task
that pins them.

An external observation trigger is a copy of the read-only sensor interface
`world_watch`: its descriptor declares `copy_of: "world_watch"`, its `life.json`
imports the owner's `world_watch_life.py` (with `allow_network: false` and
explicit `authority.read_paths` for local `file` watches), and its `probe.json`
configures an `http` (`GET`/`HEAD` poll), `file` (local file poll), or `webhook`
(local/tunnelled HTTP push listener defaulting to `127.0.0.1` with constant-time
`hmac_sha256`/`bearer`/`header` auth) source, extraction (`text`, `json` with
RFC 6901 `json_pointer`, or `status`), and trigger mode (`on_change`, `on_match`
with predicates and optional recovery, or `always`, plus optional
`notify_on_error` and `hold_while_working`). When the condition holds it sends
`watch.event` (or the configured `label`) marked `untrusted_external_data: true`
to every run whose checkpoint pins the folder.

## Add a worker

1. Copy a `system_worker_<name>/` folder.
2. Change the id in both JSON files.
3. Edit the ceiling: the interfaces it may reach (providers, doors, other
   workers), its tools, its room kinds, the marks it may close.
4. List its `role_artifacts`, creating new artifact folders for the role text.
5. Allow the new pin wherever an EE template restricts `allowed_interfaces`.

A tool that lives in a room must be reachable through one of the pin's room
kinds, and every mark id must exist, or loading fails closed.

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

See `registry/tool/GUIDE.md` and [TOOL_REGISTRY.md](TOOL_REGISTRY.md).

## Add a check and an outcome

1. `registry/mark/<id>/mark.json` - the vocabulary entry.
2. `registry/verifier/<id>/` - `verifier.json` and a script that reads the
   request on stdin and prints `{"passed": …, "details": …}`.
3. Allow the mark in `ceiling.mark_outcomes` of the workers whose checkpoints
   may close it.
4. Reference both from a checkpoint's `required_verified_artifacts`.

Do not substitute verifiers for marks or the other way around. A blank check is
the explicit `artifact_exists` verifier, never a hidden pass. See
[VERIFIERS.md](VERIFIERS.md).

## Add a room

1. If an existing executor family fits (process execution, browser, desktop,
   container, MCP), write a `worker_environment_kind` descriptor over it and a
   door interface whose driver belongs to that kind.
2. Declare the boundary ceiling in `boundary_defaults`. Policy can narrow it
   and nothing can widen it.
3. Declare new symbolic root slots, and bind them once in `host.json`.
4. If no family fits, write the executor as kind-owned package code first.

See `registry/worker_environment_kind/GUIDE.md` and
[ENVIRONMENTS.md](ENVIRONMENTS.md).

## In the client

The desktop client's **Applications** catalog presents packages as providers,
connectors, plugins and context artifacts. Installing a connector installs its
interface files; making it available in a Project and connecting it to a task
are separate steps, and only connected tasks receive its events. A plugin is a
service the model calls through its interface, such as an MCP server, together
with the context artifact that teaches the model its contract. An application
pack groups packages for setup without a new runtime identity.

**Connect** is client-local setup, like signing in to a provider. An interface
declares a connection module in `private_config.client_connection`: an exact
`interface://<owner>/packages/<path>.cjs` that exports `probe`, `connect` and
optionally `disconnect`. A module may live in a family owner's folder when that
owner declares it in `family_modules`; the Application's package then names the
owner's package in `required_packages`. The module checks what the operator
entered against the real service and returns `connected`, `not_connected`,
`expired` or `error` with an account label, a message and optionally a `manage`
link. The label names the account the interface works in (the Gmail mailbox, the
Telegram bot); the sign-in it is reached through is only the detail. The result
is presentation: it creates no run or ledger evidence, and the interface's own
life still fails closed on bad credentials. Without a module, the required setup
fields are the connection. `application.json` may add `connection.disclosure`,
`connection.help` and a placeholder and help line per setup field.

The MCP client package shares two such modules: `connection.cjs` for servers
reached with a key or OAuth (the account comes from
`private_config.account_profile`) and `chatgpt_apps.cjs` for apps linked to the
ChatGPT account (the connector comes from `private_config.connector_link`).
Connect turns the tools a server lists into operations when the interface
declares `server_tool_operations`, keeping the server's annotations, and
rewrites the catalog artifact `artifact-operation-catalog-<interface>` from the
whole `operations/` folder. Connected tasks bind that catalog, so they see tools
published after they were created.

A task, an Agent and Home are offered an application only when their worker
pin's ceiling holds it. Nobody keeps that list by hand: installing, removing or
repairing a package writes the applications' interfaces, tools and room kinds
into every worker pin its package marks `application_host`. Making an
application available in a Project admits it within the EE chain and never
widens a pin. For optional room drivers, a host opts in through its **source**
ceiling: installation adds only the named driver members, removal drops them,
and an unrelated installation does not restore a locally narrowed driver.
The driver still has to be selected on the run.

Connected tasks share the application's one interface: one life, one session,
one set of credentials. A call's result reaches only the calling task, checked
by the `tool_result_correspondence` verifier. Approval follows the task's
access, as in Codex: a Full access task carries `approval_policy: "never"`; a
Project access task carries `"on-request"`, and a tool that is not declared
read-only, or both non-destructive and closed-world, waits for the operator's
answer, checked by the `operator_approval` verifier, before the application
runs it.

Client Extensions add widgets, tabs and inputs to the app. They are client
packages in the Home's `client/extensions`, not registry descriptors, reviewed
by the owner. A run writes one with its granted file tools after reading the
client's contract through the `inspect_client` client action. A view never gains
runtime authority: system data reaches it through a feed interface that a run
emits into.

## Who may change the registry

Authority is policy, and the mechanism is ordinary file tools. A run whose room
covers a registry path can write there; the write is a gated, recorded tool
call. A narrower run asks by emitting an artifact to a run that already holds
the authority. There is no mutation API and no delegated authority.

## Before adding code

- Is there a registry or descriptor layer where this behavior belongs instead
  of a branch?
- Does it branch on a worker's name, a provider's name, a model string or an
  artifact label? Then stop.
- Does every new side effect leave an artifact and a movement?
- Is failing closed with a clear error better than a fallback that almost
  works? It usually is.
