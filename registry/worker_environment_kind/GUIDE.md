# Writing a worker_environment_kind

A worker environment is the technical **room** where a tool's effect
physically happens: a shell, a git worktree, a browser session, a remote
machine. It is needed **only** when a run needs tools that act on the world; a
pure transformer interface (text-to-speech: artifact in, artifact out) has no
room. A room is reached through its **door** interface: a tool call arrives as
an artifact, the room executes it, and returns a result artifact.

A `worker_environment_kind` is the technical descriptor of such a room - the
template. A live worker environment is leased for a run. A kind carries no
goal semantics (that is the EE axis) - only substrate, tools, boundary, and
lifecycle.

Background: [Environments](../../docs/contracts/ENVIRONMENTS.md).

## Where it goes

```text
registry/worker_environment_kind/<id>/worker_environment_kind.json
```

The folder is the identity: its name equals `worker_environment_kind_id`.
Loading is fail-closed, unknown fields are refused, and there is no
`schema_version` in the body.

A kind's code lives **in its own kind's folder**:
`registry/worker_environment_kind/<kind name>/packages/*`. A kind's folder name
is the name of an executor **type**, not a routing id - "id" is interface
vocabulary, where artifacts are delivered by it; nothing routes by a kind's
name. Each kind has **its own** door life and executor processes and does not
use another kind's live executable. Code, however, is reused by explicit
import from the owning kind's folder, as with interfaces; byte-for-byte clones
of code per kind are not the model.

Every door life and tool process is built on the door runtime that
`process_session` owns (`octopus-process-session-tool-runtime`, imported by
the other kinds as `octopus-tool-door-runtime`): the ledger cursor, the grant
and formation checks, recovery, and the small room glue every door needs -
decoding and admitting a `tool.call`, matching its coordinates to the
formation, reading the descriptor's operation, emitting result artifacts. A
kind's package keeps only what its room does.

A door life and its tool processes are applications of the OS, not part of
the kernel: they link neither `octopus-runtime` nor `octopus-app-server`. Run
records are read through `octopus-run-record`; a checked run write goes through
the authoring package that `system_interface_authoring` owns.

The box crate (`crates/octopus-worker-env`) holds only the descriptor
dictionary, the fail-closed registry and formation, a generic lease, and the
`WorkerEnvDriver` trait with its surface traits and authorization gate. The
room's **behavior** - boundary projection, materialization, patch, exec,
worktree - lives in the kind's package.

## Required fields

```json
{
  "worker_environment_kind_id": "git_worktree",
  "substrate_family": "process_exec",
  "door_driver_refs": ["system_coder_env"],
  "tool_process_bins": [
    "target/debug/octopus-coder-exec-tool",
    "target/debug/octopus-coder-fs-tool"
  ],
  "worker_consent": "explicit_ceiling",
  "supported_executor_kinds": ["exec"],
  "capabilities": ["filesystem", "process_exec", "git"],
  "boundary_defaults": {
    "read_root_refs":  [{ "path": "repo_root", "access": ["read"] }],
    "write_root_refs": [{ "path": "worktree_root", "access": ["read", "write"] }],
    "protected_read_only_subpaths": [{ "path": "protected_branch_ref", "access": ["read"] }],
    "network_ceiling": "full"
  },
  "lifecycle": { "requires_release": true }
}
```

The optional `instance_config_files` lists strict JSON-object sidecar files in
a concrete door's folder. Formation reads and pins the hash of each listed
file; a missing, extra, or malformed config fails closed.

## Rules

- **`substrate_family` names the code family that serves the kind.** The tool
  process behind a door declares the family its package implements, and the
  door runtime refuses every invocation and release whose formed kind names
  another family before the package sees it. A package that opens its room
  through the box's generic lease hands it a `WorkerEnvDriver` of the same
  family, and the lease checks that too. The box keeps no family registry.
  Kinds share a family only when they run the same room code: `git_worktree`
  and `filesystem_workspace` are both `process_exec` because the worktree
  package imports the workspace room from `filesystem_workspace` and adds its
  git layout and worktree operations to it. `capabilities` are descriptive
  vocabulary for policy and presentation; no code branches on them.
- **A new kind is a folder with its own packages,** never a descriptor alone:
  its door life names the kind it serves, and every `tool_process_bins` entry
  is a binary built from a package in the kind's folder. When the room code
  already exists in another kind, those binaries are a few lines that import it
  from the owning kind's folder.
- **`door_driver_refs` is the static ownership of doors by a code-world.**
  These are driver refs, not the exact interface ids of concrete tasks or runs.
  An exact door remains a separate System-interface folder; its
  `interface_driver_ref` must appear in this list. An unknown, non-executable,
  or foreign door fails closed.
- **`tool_process_bins` is the static ownership of executables by a
  code-world.** These are package-owned process binaries, not a list of tool
  ids. Every tool that names this kind through
  `supported_worker_environment_kinds` must have a `process.bin` from this
  list, and every declared binary must be used by at least one such tool. Both
  directions are checked fail-closed.
- **`worker_consent` has a single value, `explicit_ceiling`.** Exact worker ids
  are never listed in a kind. A concrete worker pin allows a kind only through
  its own `policy.json → ceiling.wek`; formation admits a kind only if it
  survives in the run's formed grant.
- **Choosing a room is the intersection of four explicit links:** the formed
  worker grant contains the kind; the tool descriptor names the kind; the exact
  door has a declared driver ref; the tool has a declared process binary. Zero
  candidates or more than one is an error. There is no hidden default.
- **Instance configuration belongs to the door's folder, not to the kind.**
  Slot values are two layers of data: `host.json` holds machine defaults for
  every declared slot (one file per machine, for all doors; a new kind declares
  new slots in `boundary_defaults` and you bind them there once), and a door
  interface's folder may override any slot per instance - ten sessions are ten
  folders with pinpoint overrides. JSON only, no code.
- **`boundary_defaults` is the sandbox ceiling, which policy can only
  narrow.** There is no widening operation. Roots are symbolic: a slot
  (`repo_root`, `worktree_root`) resolves when the room is instantiated. When a
  room is leased, its capabilities are derived from this ceiling; there is no
  permissive seed on the live path. `network_ceiling` narrows by minimum
  (`denied < restricted < full`). Projecting the boundary from slots to
  concrete roots is the kind package's math; the box holds only enforcement.
- **`lifecycle.requires_release`** - the room needs a release hook on teardown
  (worktree removal, container stop). A driver that cannot provide the hook to
  such a room will not open it: a local process driver does not pretend to be a
  remote machine. When the live owner is gone, a fresh tool process releases
  the room cold only if its package declares that its release works from the
  formation alone; otherwise the cold release fails closed.
