# Workers

If you come from other agent frameworks: a worker is your agent. A coding
agent is a coder worker, a browser agent is a web worker. What changes is that
the agent stops being a program. The loop it used to own is expressed by the
architecture, and what remains is a formation: a few JSON lines saying which
interfaces, tools, rooms, and proofs it may ever hold.

**A worker is a System interface**: a unique pin
`registry/interface/system_worker_<name>/` holding an ordinary interface
descriptor and a `policy.json` ceiling. Workers are found the same way every
other interface is: by the folders on disk.

A worker is not a unit of code. In a run there are only interfaces, policy,
bindings, artifacts, and checkpoints, and "worker" is the name for that bound
configuration in motion. Because it is only bindings plus policy, a worker can
be fully model-driven or have no model behind it at all.

## The pin

`interface.json` is the pin other runs address, bind, and emit to:

```json
{
  "id": "system_worker_coder",
  "interface_type": "system",
  "role": "worker",
  "direction": "bidirectional",
  "source_names": ["coder"]
}
```

`policy.json` is the ceiling - the static maximum any run wearing this worker
could ever hold - plus the artifacts that ground its role:

```json
{
  "id": "system_worker_coder",
  "ceiling": {
    "interfaces": ["world_provider_codex", "system_coder_env", "system_workspace_env", "system_room", "…"],
    "tools": ["coder.read_file", "coder.apply_patch", "coder.build", "artifact.read", "…"],
    "wek": ["filesystem_workspace", "git_worktree", "system_room"],
    "mark_outcomes": ["coder.build.ok", "operator.approval"],
    "hard_blocked_tools": [],
    "artifact_verbs": ["read", "create"]
  },
  "role_artifacts": ["artifact-codebase-layout"]
}
```

(Abbreviated. The shipped pin lists every provider interface it may be bound
to.)

Loading is fail-closed both ways. A System interface with `role: "worker"` and
no `policy.json` refuses to load: a pin without a ceiling forms no grant. A
`policy.json` beside a non-worker interface is descriptor drift. Unknown fields
fail to parse.

## A new worker is a copied folder

Duplicate an existing `system_worker_<name>/` directory, change the id, edit
the lists. Two JSON files and no Rust changes. A worker has no code of its own. At runtime it is its run's artifacts
crossing its granted interfaces under its formed policy.



## How a run wears a worker

A run record names its pin:

```json
{ "id": "run_42", "ee": "ee_project_x", "worker": "system_worker_coder", "...": "..." }
```

Formation resolves the ceiling by that id:

```text
grant = worker-interface ceiling  ∩  EE chain  ∩  the run's authored bindings
```

One gate runs before any grant exists: `run.worker` must be inside the
interface allowance of every EE layer in the run's chain. A run record claiming
a powerful pin inside an EE that does not list it fails closed. A run cannot
lift itself above its place in the tree.

## Reaching another worker

There is no generic worker interface. A run reaches a coder by holding
`system_worker_coder` in its own ceiling's `interfaces` and authoring a binding
to it on a checkpoint. "Which workers may this worker reach" *is* the set of
`system_worker_*` ids in its ceiling - one axis, the interface axis, with
nothing parallel to it.

EE templates express worker allowance the same way: `allowed_interfaces`
containing `system_worker_*` ids.

## Which worker handles an arrival

An artifact arriving at an interface wakes whichever run pins it. Which worker
that run wears is authored in the run record. All pins are equal: none is an
entry point for the others.

A worker's tool surface is its formed policy. A run whose worker has no
modifying tools hands deeper work on by emitting an artifact through the
target's own pin binding, admitted by policy.

Ceiling interface ids describe potential reach, not installation requirements.
Minimal packs may leave some targets uninstalled. Startup summarizes those as
informational; installed targets rejected by registry validation remain warnings.
Neither class is available to active bindings: an unavailable target remains
denied until its descriptor is installed and valid.

## Role artifacts

A worker's role and prompt are not a field and not a file inside the pin. They
are the content of bound context artifacts. `role_artifacts` lists the ids that
ground the role; run records bind them onto checkpoints; the interface carries
them into the model's context. The same generic path for every worker, never a
branch on a worker's name.

## Tools, rooms, marks

- `ceiling.tools` - the tool surface. Every id must resolve to a tool
  descriptor, and a tool that lives in a room must be reachable through one of
  the pin's `ceiling.wek` kinds. What the model is offered is exactly the
  formed grant's tools.
- `ceiling.wek` - the worker-environment kinds this pin may use. A kind never
  lists worker ids; it declares that it requires explicit consent, and
  formation admits it only if it survives in the worker's formed grant.
- `ceiling.mark_outcomes` - the marks this pin's checkpoints may close. A
  ceiling that references a mark id absent from `registry/mark/` fails closed
  at boot and on live reload. Workers do not emit marks; verifiers admit them.

## Models

A model is an **interface**, not a worker field. Which model a run uses comes
from the interface bindings authored on its checkpoints, cut by the ceiling.
Switching provider is a couple of JSON lines on the run, never a worker edit.
The boundary is one-directional: a pin's ceiling names provider interfaces; a
provider interface never names workers.

## Delegation is authored contracts

Any run whose grant lets it author run records can hand work to another
worker. Planning is model work expressed through those records.

A plan is the set of run and checkpoint contracts that run authors: for each
step, a run for some worker pin with a concrete task, the artifacts it waits on
before it starts, and - where proof is worth it - the
`required_verified_artifacts` it must close. The dependency graph is those
awaited artifacts.

Steps with nothing left to await run in parallel. A dependent step waits as
checkpoint data until its predecessors' artifacts arrive, and their marks are
admitted where they are required.

## In the app

An **Agent** in the desktop client is a user-facing name over a reusable worker
template: an exact pin and ceiling plus authoring defaults for a new task.
Creating an Agent creates no run. Starting a task with it creates a new run in
that Agent's project. The client lists only Agents someone explicitly created;
it does not turn every worker pin into one.
