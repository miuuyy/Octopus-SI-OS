# Interfaces

Interfaces and artifacts are the skeleton of the framework. Any API call or
external event is a transaction of artifacts across a declared interface.

## Identity is the folder

```text
registry/interface/<id>/interface.json
```

The folder name is the interface's id, and the id is the routing and policy
identity. Nothing else addresses an interface.

```json
{
  "id": "world_pyecho",
  "interface_type": "world",
  "role": "transformer",
  "direction": "bidirectional",
  "source_names": ["pyecho"],
  "interface_driver_ref": "pyecho_life"
}
```

- `interface_type` - `system` or `world`. World is a boundary with something
  outside the system's control; System is an internal one.
- `role` - a classifier the interface and policy surfaces may read: `provider`,
  `worker`, `service`, `sensor`, `transformer`, `user`, `system`, and so on.
- `direction`, `source_names` - the interface's declared direction and its own
  source vocabulary.
- `interface_driver_ref` - the driver family this interface's code belongs to.
- `interface_driver_config` - data for the interface's own code: transport,
  model, retries, labels. The kernel never interprets it.

**Classifiers do not route.** `interface_type`, `role`, `direction`, and
`source_names` describe one exact folder. They never choose a route, create a
family wildcard, or widen a binding. Do not split a folder for a payload
variant, and do not merge two conversations because their classifiers match. A
private conversation or any isolated boundary is a new folder. Its JSON may
reuse code from another interface and it remains an equal citizen.

The [System/World distinction](../architecture.md#system-and-world) describes
control. World-facing ports do not form a second execution graph outside
System. A movement can name two World-facing endpoints, such as a provider
and an operator interface; that transfer still passes through System under
the run's checkpoint bindings and formed policy. It is not direct coordination
inside World, and `interface_type` alone neither permits nor forbids the route.

Checks such as "is this MCP", "is this local", or "is this a model" do not
exist in the runtime. MCP is not a framework layer; an MCP server is one
interface among the rest.

## Delivery is a ledger movement

Artifact transfer is a full protocol through the runtime on both legs: the
grant at the gate, the movement in the ledger, the verifier on the edge. The runtime keeps no queue, claim handle, or ack.

**Inbound.** An interface hands its artifact to the runtime. The hand-in handle
*is* the interface binding. The verifier on the edge runs before the movement
is recorded.

**Outbound.** The runtime re-reads the current run record, forms outbound
transfers from the checkpoint's interface bindings, and records each one. The
receiving interface is a live reader of its own movements from its own cursor.

**Nothing is lost.** A dead interface catches up from its cursor when it wakes.
An unread result is an ordinary consequence of the architecture, not a loss to
be repaired with leases or retries. If one interface holds two bindings, two
artifacts arrive - one movement per binding.

The runtime scans descriptor JSON to know what is available. It never scans
folders to "see" an artifact.

## An interface that lives

`interface.json` is enough for a descriptor-only interface. A worker pin is
just its descriptor and `policy.json`. An interface that needs to run declares
a process beside it:

```text
registry/interface/<id>/life.json
registry/interface/<id>/<its code>
```

```json
{
  "processes": [
    {
      "bin": "/usr/bin/python3",
      "args": ["registry/interface/world_my_channel/life.py"],
      "required_files": ["registry/interface/world_my_channel/life.py"],
      "inherit_env_from_interface": ["/interface_driver_config/transport/auth_env_key"],
      "authority": {
        "read_root_slots": [], "write_root_slots": [], "executable_root_slots": [],
        "read_paths": [], "write_paths": [], "executable_paths": [],
        "executable_commands": [],
        "read_env_paths": [], "write_env_paths": [], "executable_env_paths": [],
        "allow_network": true, "allow_local_ipc": false
      },
      "read_handles": ["ledger", "blobs"]
    }
  ]
}
```

The runtime does not call interface code and does not select a compiled family.
`life.json` chooses the executable. The process starts fail-closed with a
cleared environment and exactly the authority it declares: its own folder, its
own writable state directory, a private stdin/stdout channel to the runtime,
and the handles it lists. The ledger, the blob store, and the index are
available only as explicit read handles. In the default enforced sandbox mode,
undeclared access is denied. A descriptor can explicitly disable that OS
sandbox; see [Security model](../security-model.md#two-independent-layers).

**Wake.** Interfaces wake with the runtime. During its ordinary scan of
registry and run records, the runtime wakes every interface mentioned on a live
run binding. Liveness comes only from bindings: an installed interface nobody
binds does not run. From then on the interface lives by itself, already warm.

**Sleep.** A life that has nothing to do — its own code decides: no turn open,
no call in flight, nothing awaited — may end itself. It first prints, after it
has declared readiness, one last frame naming the first movement it has not
read, then exits with status 0:

```json
{ "kind": "asleep", "through_seq": 41328 }
```

That is sleep, not coma: no failure is recorded and no backoff applies. When
the runtime records a movement for that folder at or past `through_seq`, it
lifts the life again, and the life catches up from its cursor. A frame after
`asleep`, or `asleep` before readiness, fails the life closed. A life that
listens to the world (a clock, a mailbox, a chat) never prints it.

**Hand-in.** The process prints one JSON line per artifact:

```json
{
  "kind": "arrival",
  "request_id": 1,
  "artifact_manifest_path": "/absolute/private-state/output.artifact.json",
  "evidence": []
}
```

The runtime answers on stdin with a `delivery_verdict` carrying the same
`request_id`. A System-interface emission uses `kind: system_emission` and names
the emitting run, checkpoint, and both interfaces; the runtime still re-forms
the grant and may refuse.

**Cursor.** A cursor is this reader's bookmark in the append-only ledger. It is
committed after an admitted output or an admitted failure artifact. A crash, an
unavailable intake, or a transient failure does not move it.

The full authoring guide is
[registry/interface/GUIDE.md](../../registry/interface/GUIDE.md).

## Reusing code without sharing identity

Executable code belongs to a concrete owner folder:
`registry/interface/<id>/packages/`. There is no neutral shared code home.

An interface addresses its own executable as `owner://packages/...` and
another interface's as `interface://<source-id>/packages/...`. The import
creates no shared identity, no family admission, and no policy surface. It is
implementation reuse only. Every folder still carries its own complete JSON and
is named by exact id in the policy that admits it, with one exception: a
private copy may declare `copy_of`, the folder it was copied from, and policy
that names that original admits the copy, formed no wider than the original
(see [Policy](POLICY.md)).

A new executable instance declares its own `life.json` and complete descriptor
and configuration. It may import another owner's executable explicitly, but
it does not inherit that owner's identity, policy, or configuration. A
`reference` field on `interface.json` is rejected by the loader.

Installing a capability materializes its owner folder and dependencies in the
active registry. **An interface is an app.** The current client installs from
a bundled local package library; this does not imply a hosted download store.
See [package installation in the client](../client.md#where-packages-come-from).

## Providers are drivers

A model provider is installed exactly like a device driver on an OS: a
descriptor plus driver code owned by a concrete interface folder, never a
kernel edit. The kernel speaks one contract and stays blind to two things: the
payload format, and which provider is on the other side.

- **The contract.** The provider interface receives a request artifact with its
  bound context and returns artifacts. Decoding function calls versus content
  lives entirely in the driver. It emits each outcome as an ordinary artifact
  into the binding that carries it: a tool call toward a room's door, a reply
  toward the run's outbound binding. One API response may carry many tool
  calls; the driver emits many artifacts, each passing the gate independently.
- **Selection is a lookup, never a brand match.** A run reaches a provider
  through formed policy and an interface binding. Nothing in the runtime matches on a model name.
- **Transport is descriptor data.** Base URL, wire dialect, headers, timeouts,
  and the *name* of the environment variable holding the secret all live in the
  interface's descriptor. Only the bound driver reads them.
- **A provider over an existing wire is descriptor-only.** The OpenAI-compatible
  driver already drives any compatible endpoint - a hosted API, a local Ollama
  or LM Studio server, a router. A new provider is a new self-contained folder
  that imports that driver and declares its own transport.
- **A new wire is one driver, then data.** A genuinely different dialect is new
  driver code written once, inside the folder that owns that wire. After that,
  every provider over it is a descriptor.

Context assembly belongs here too. There is no context builder in the runtime:
the provider interface renders the bound artifacts and the grant projection
into the prompt and the tool list.

## Secrets

Secrets and tokens are not registry data. Each interface owns its environment
files, keys, tokens, and login logic inside its own private state. There is no
shared secret manager in the runtime. The value stays behind the boundary; what
a run can see is a handle (`secret://...`) carried as binding evidence. A
handoff that depends on a secret carries the handle, not a paraphrase of "the
token".

## Activation

An interface that must listen for external arrivals may carry an
`activation.json` with host configuration for its own driver. It cannot start
or keep alive an interface that no run binds, and malformed activation data
fails closed for that interface without blocking healthy siblings. Editing
configuration or bindings is picked up live.

## Any language

Because only an artifact crosses the boundary, a driver's language is invisible
to the kernel. An interface in Python is its own process with a `life.json`,
artifact files out, and its movements read back from the ledger or from the
SQLite index, which opens natively everywhere. `world_pyecho` in the registry
is the smallest working example.

## The client

The desktop client is itself interfaces plus a file explorer over the
registries. It acts by handing artifacts to declared interfaces, and otherwise
reads descriptor and artifact files, exactly like a file manager. An interface
folder may carry `application.json` to present itself as an Application in the
app; that file is presentation and grants nothing.
