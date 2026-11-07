# Environments

"Environment" means two different things in Octopus. They are not
interchangeable and they do not nest under a third concept.

| | Execution environment (EE) | Worker environment |
| --- | --- | --- |
| Answers | Why, and what is allowed | Where the effect physically happens |
| Owns | Goal, policy, place in the hierarchy | Executor, tools, sandbox boundary |
| Declared in | `registry/ee_template/` | `registry/worker_environment_kind/` |
| Lives as | `state/ee_instances/<ee_id>.json` | A leased room behind a door interface |
| Needed | Always - every run lives in one | Only when tools must act on the world |

## The semantic axis: EE

An EE is a scope classified by a goal. The system is defined by the full set of
EEs it can act in.

- **Template** - `registry/ee_template/<id>/ee_template.json`. Declares the
  goal class, the interface allowance (which includes worker pins), allowed
  room kinds, optional tool narrowing and hard blocks, the child templates that
  may be created beneath it, and the host slots its rooms may be given.
- **Instance** - a JSON record with its stable ID, optional human-readable name,
  template, parent, goal text, and any further narrowing.

```json
{
  "ee_id": "ee_project_df8954674cf6fca310f23412",
  "name": "Life",
  "template_name": "project",
  "parent_ee_id": "ee_existence",
  "goal": "Manage personal work and commitments",
  "config": {},
  "policy_narrowing": {}
}
```

`name`, when present, must be nonblank. It is EE metadata, independent of the
goal, template and policy; `ee_id` remains the stable identity. The client writes
the Project name into this field when creating or explicitly renaming a Project.
Topology and Project lists read the EE record's name directly. An unnamed EE is
shown by its ID, without borrowing a goal, template or presentation alias.

**Environments form a hierarchy.** An instance may have a parent, and policy narrows
inward along that chain. A parentless instance starts an independent chain. The
single `bootstrap_root` template selects the initial seed; other authored roots
inherit nothing from it. Creating or reparenting an EE requires a granted path
to its record. See [Policy](POLICY.md).

**Reorganizing is JSON.** Allowances name originals, and a task's private
copies are admitted by them, so a narrowed environment can host real runs: a
memory service in `memory_maintenance`, a recurring line of work in its own
project. An environment that lists interfaces names every original a task
carries: its worker, provider and client, the rooms it uses, and the service
doors composition adds (`system_service_input`,
`system_worker_memory_observer`, `system_context_memory_projection`). Worker
templates that host a provider hold the first two in their ceilings, so a
task's copies keep them when narrowed; a refusal names the missing original. A run lives in the one environment its record's `ee` names; another
run moves it by changing only that field, and the new place applies from the
run's next arrival.

**Goals classify, they do not enumerate.** The root goal is `existence`. The
registry ships templates such as `operator_space`, `project`,
`operator_interaction`, `memory_maintenance`, `self_improvement`. These are
registry entries, not reserved constants. One request stays one goal unless a
policy or outcome boundary shifts; the boundary follows the meaningful outcome,
not every step of a task.

**One primitive for long-running and one-shot.** An
EE exists while its record and relationships exist. Stopping, deleting, or
unlinking one is an ordinary authorized file change.

**An EE boundary is a policy boundary first**, not a directory or a process.

Creating a goal is a cognitive act inside a run, performed under policy. It is
not a pipeline transition. When to split a goal and when to keep it merged is
open research; the framework has to prevent useless infinite splitting while
preserving real outcome boundaries.

## The technical axis: worker environment

A worker environment exists **only where a run needs tools that act on the
world**. It is the room those effects happen in and the thing that provides the
tools. A run that only transforms an artifact - text in, audio out - needs
none; it is just an interface.

It owns the executor, capabilities, the boundary ceiling, and release. It owns
no goal semantics.

The registry ships these kinds:

| Kind | Room |
| --- | --- |
| `filesystem_workspace` | A real project folder, used directly. The default for local tasks. |
| `git_worktree` | An isolated worktree on its own branch. Chosen explicitly. |
| `process_session` | A persistent process with a PTY and a private workspace. |
| `browser_session` | A persistent browser. |
| `desktop_session` | Screen capture and input - computer use. |
| `container_session` | A container. |
| `remote_exec_server` | A remote machine. |
| `mcp_stdio_session` | A stdio MCP server. |
| `code_mode` | Script cells that emit nested artifacts. |
| `web_search` | Web search. |
| `audio_device_session` | Local audio capture and playback. |
| `system_room` | The system's own files: run records, registries, artifacts. |

A project's path is an ordinary host-slot binding. A standard local task works
in a `filesystem_workspace` whose root is the real project folder; nothing
requires a second checkout to read or change files. `git_worktree` is a
specialized room for isolation you ask for.

## Three parts, one code-world

A worker environment is **exactly three parts**:

1. the **kind** - the executor that defines the possible tools;
2. its **door interface** - the System interface a tool call is sent to;
3. the **policy** the runtime forms to materialize it.

Its conceptual boundary is the **code-world**. The kind's executor, its tools,
and its door live in one language, one process world. A tool's code belongs to
the same code-world as its kind's executor - never to the interface that asked
for it. Only artifacts cross out.

That shared code-world is why the worker environment is a separate primitive,
and it is what makes polyglot rooms possible: a Python scraping room is a
Python kind, Python tools, and a Python door. The runtime does not know and does
not care.

A kind's code lives in its own folder,
`registry/worker_environment_kind/<name>/packages/`. Each kind runs its own
door and executor processes; code is reused between kinds by explicit import
from the owning folder, the same way interfaces reuse driver code.

A kind's folder name is the name of an execution-code type, not a routing id.
Artifacts are delivered by interface id; nothing routes by a kind name.

## Persistent command control

The workspace keeper retains invocation and exact per-call results outside the
command's filesystem scope. Its process descriptor may explicitly grant
`allow_process_signals` to the trusted executor. On macOS this permits signals
across sandbox labels; the default is false, and a copied descriptor cannot add
that authority. Command arguments still cross their own formed sandbox before
execution and do not inherit the executor's signal authority. Command control
accepts an opaque process identity belonging to the formed WorkerEnvironment,
never an arbitrary host PID supplied by the model.

Private tool copies retain their authored authority. A distribution release with an incompatible contract
requires fresh composition in a fresh Home, without rewriting existing copies or
accepted invocations. No already accepted invocation gains new rights.

A failed stop retains the process handle and reports the actual error. Closure
requires observed exit, stream closure and termination of the owned process group
within the declared cleanup timeout;
a deadline or permission failure never synthesizes an exit. A successful retry
clears the current termination failure only after closure, while earlier call
receipts remain unchanged. Replacing a package does not change the sandbox of
an already running keeper. A keeper creates only its own record directories
inside a process registry that still exists; once the Home that owned it is
gone, retiring a closed command fails instead of recreating the tree.

A path outside the formed roots is refused as a boundary violation. A
filesystem failure inside them (a missing file, a permission the operating
system denies) names its operation and path instead, so it never reads as a
policy refusal.

On macOS, the executor observes root exit without reaping its PID until the owned
group has finished. Signals and reaping share that ownership; after it is
released, later control cannot target a reused numeric process identity. Root
exit remains distinct from closure, including a PTY whose streams close while
another group member is still running. Observation and cleanup failures remain
visible and never become successful closure.

## Choosing a room

Selecting a room is the intersection of four explicit links:

- the run's formed grant contains the kind;
- the tool descriptor names the kind;
- the exact door interface belongs to the kind;
- the tool's process binary is owned by the kind.

Zero candidates or more than one is an error. There is no hidden default.

A kind never enumerates the workers allowed to use it. It requires explicit
consent, and a worker grants that only through its own ceiling.

## The boundary can only narrow

A kind declares `boundary_defaults`: the sandbox ceiling for its rooms  - 
read roots, write roots, protected sub-paths, a network ceiling. Policy can
narrow that boundary. There is no operation that widens it.

Roots are symbolic slots such as `repo_root` or `worktree_root`. They resolve
from two layers of plain JSON, no code:

1. `host.json` - this machine's defaults for every declared slot. One file per
   machine, for all doors.
2. The door interface's own folder may override any slot for one instance. Ten
   sessions are ten folders, each overriding only what differs.

The runtime never reads these as a decision surface. The door resolves them as
data.

## The path is the permission

Access to a directory is the right to change its files. This is how run-state
and descriptor changes work without any special machinery: to let a run edit
its own record, an EE's JSON, or a registry folder, policy grants it a room
whose path covers that file. The edit is an ordinary file tool call - gated,
recorded, and executed like any other.



A door's host-slot override carries `path` and may declare `additional_paths`.
The primary path remains the working directory. Additional paths extend that
same symbolic slot to an explicit set of roots; they never grant their common
parent. Process authority, filesystem boundaries and protected subpaths apply
to every root under the slot's existing grant. The formed record retains the
additional bindings and includes them in its formation pin; a lease refuses a
different set. Empty additional bindings are omitted, preserving existing
single-root records and their pins.

## Fail closed

- A malformed EE or template identity fails before any state is written.
- A missing EE on a live run is an error, not an invitation to mint a default.
- A room whose kind requires release will not open under a driver that cannot
  provide the release hook.

The authoring guide is
[registry/worker_environment_kind/GUIDE.md](../../registry/worker_environment_kind/GUIDE.md).

## In the app

A **Project** is an EE. A **Driver** is a worker-environment kind the operator
can install and make available to a project. Installing its package does not
create a room or grant its tools to every run. A task obtains an environment
through its kind, door interface, and formed policy.

For example, **Filesystem Workspace** supplies tools in a chosen folder;
**Git Worktree** supplies an isolated checkout. Files, Terminal, and other
task panels reflect the actual environment's capabilities. These are choices
made by the [optional desktop client](../client.md), not extra runtime primitives.

![Drivers catalog showing local execution environments and their installation states](../images/client/drivers.png)
