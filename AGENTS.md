# Octopus

Octopus is an experimental agentic operating system built for long-horizon,
open-ended autonomy. A small Rust kernel admits artifacts, forms policy, and
records movements. Everything else - model providers, channels, tool rooms,
workers, memory, the desktop client - attaches to it through JSON descriptors
and one boundary contract.

Codex was the first provider ported onto it and backs voice and initial sign-in;
Home defaults to Gemini 4 Neon via Antigravity. The desktop app is the product
surface: Home, Projects, Agents, Tasks, Applications, Drivers.

Read [README.md](README.md), [docs/architecture.md](docs/architecture.md), and
[docs/development.md](docs/development.md) before non-trivial work. For work
that touches a primitive, read its page under
[docs/contracts/](docs/contracts/README.md), starting with
[POLICY.md](docs/contracts/POLICY.md). These public guides, the owning code,
the schemas, and the current diff are the portable context for this
repository.

## Core principles

### 1. The kernel is small and blind

About 62,000 lines of Rust in `crates/`. It knows no provider, channel, tool,
model, or artifact by name. Keeping it small is the point of the research: every
capability added by declaring something instead of coding a branch is one more
thing the system can inspect, replay, and change about itself.

### 2. Capabilities live in the registry

Thousands of JSON descriptors are intended. Interfaces,
workers, environments, tools, verifiers, and marks are data the running system
reads live and can rewrite under policy. If adding one channel means editing
app bootstrap or a manager named after the concept, a primitive has leaked.

### 3. Effects leave evidence

Every autonomous effect crosses a declared interface under policy and leaves a
movement in the ledger. Marks come from verifiers, never from a model's claim.
Replayability is what keeps a long-running system's picture of itself
accurate.

### 4. The surface stays simple

Under all of this, a person signs in, picks a few things, and talks to it. The
onboarding and the client are held to a product bar, not a research-demo bar.
Do not make the surface pay for the architecture.

## Design stance

I keep the design intentionally small in primitives and intentionally large in
descriptors. Octopus is not built for standard benchmarks or short, bounded
coding tasks; on those it adds nothing over a plain agent loop. It is built for
long-horizon, open-ended autonomy where persistent goals, triggers, parallel
runs, replayable evidence, and descriptor-level self-modification matter.

Execution failures here are almost always architectural: the wrong contract, the
wrong data shown to the model, a missing field. Hardcoding around one hides a
problem in the system behind a workaround for the model. Autonomous initiative
itself still depends on model alignment (currently seen mainly in recent
Gemini 4 models, especially Gemini 4 Neon), but that stays an empirical choice
of default provider, never a provider or model branch in the kernel.

If a direction here looks wrong, overbuilt, or unclear, do not quietly
translate it into a familiar agent-framework pattern and keep coding. Stop and
say so. That is not permission to ask about every discomfort - only when you
cannot implement something faithfully without an answer.

## A small glossary

The full [glossary](docs/glossary.md) is short; read it. The words that matter
most:

- **interface** - a typed port identified by its folder. Artifact in, artifact
  out. Providers, channels, room doors, and workers are all interfaces.
- **artifact** - the only thing that crosses a boundary.
- **run** - a durable execution record. Data, not a process.
- **worker** - what you would call an agent, kept as data: a
  `system_worker_<name>` interface with a `policy.json`
  ceiling. Not an engine, no code.
- **EE** - an execution environment: a goal with a policy. Runs live in EEs.
- **worker environment** - the room where tools act. Distinct from an EE.
- **grant** - what a run may do right now, formed at every arrival.
- **mark** - the recorded fact that a verifier passed.
- In the app: a **Project** is an EE, an **Agent** is a reusable worker
  template, a **Task** is a run.

## Common mistakes

**Branching on identity.** `if run.worker == "system_worker_X"`,
`if source == "telegram"`, `"gpt" in model_id`, a prefix match on a tool name.
Each looks like a fix and is an alarm that the contract is wrong. Find the
descriptor field or binding where the behavior belongs.

**Building the module named after the behavior.** If skills are not being
created, the answer is not a skill engine. Trace the native path - a finished
run, an artifact, a permitted transfer, a receiving checkpoint, a worker that
transforms it - and fix the primitive that broke. A `SkillStore`, a
`MemoryRouter`, a `SelfImprover`, a supervisor, a scheduler: these names
usually mean the framework was skipped.

**Falling back.** A silent semantic fallback, a compatibility shim, a
"try a few heuristics", a placeholder success. Fail closed with a clear error
instead.

**Routing by label.** Artifact names, interface classifiers, and sender claims
never choose a recipient. Routing is the exact interface folder and the
authored binding. Nothing else.

**Confusing the two environments.** Goal and policy scope belong to EEs. Tool
execution belongs to worker environments and their doors.

**Imposing the architecture on the client.** Onboarding, windows, file
preparation, package installation, and presentation are things the operator
does. They are the client's own business and are not routed through runtime
policy for the sake of formal consistency.

**Bringing vocabulary the framework does not have.** Heartbeat, session id in
the kernel, artifact kind, orchestrator, pipeline, agent-as-primitive. The
glossary lists them with what covers each need.

## Where code lives

- `crates/` - the kernel and its generic hosts.
- `registry/interface/<id>/` - one interface: its descriptor, and its own
  driver code under `packages/` when it has any.
- `registry/worker_environment_kind/<name>/` - one kind of room: descriptor,
  executor, and tool binaries.
- `registry/{tool,verifier,mark,ee_template,artifact}/` - descriptors.
- `app/` - the desktop client.

Code belongs to a concrete owner folder. There is no neutral shared home; an
owner reuses another owner's code by explicit import, which creates no shared
identity and no policy surface. Find the concrete owner before editing.

## Changes

Adding a capability over an existing driver or room is descriptor and binding
work. A genuinely new execution family is code inside its owner folder.
Architecture changes need the maintainer's agreement; do not invent a primitive
to fix a local symptom.

Land coherent, context-aware changes rather than many small passes. During a
cleanup, do not equate deletion with completion: identify the capability old
code carried and preserve it through the primitives, or confirm it is
genuinely unneeded.

Check the current diff and preserve unrelated work.

## Verifying

- Run focused contract checks at meaningful milestones, not after every small
  step. See [docs/development.md](docs/development.md#checks).
- Tests follow the framework. Add one contract test when a primitive is stable.
  Keep test changes under about 40% of the working diff, and do not pin
  implementation details.
- For anything a user can see, test the packaged app with its bundled runtime
  and a dedicated test Home. Do not substitute debug binaries or someone's live
  Home.
- A source check, a successful build, and an observed model reply are three
  different claims. Keep them distinct.

## Comments

Comments in code are for documented architecture only: a comment may say how
this code connects to something the public docs name, when the code already is
that way. No "should be X but is not yet", no migration notes, no explanation
of why a change is correct, no reference to a review, a log, or a campaign.

## Documentation

Most changes need no documentation change. `docs/` records decisions,
constraints that span components, and traps the source does not reveal. Before
adding a paragraph, ask what a maintainer would get wrong without it. When a
documented decision changes, rewrite the text; do not append another account.

Plans, research, journals, and logs are working material. Keep them in ignored
paths. Do not touch private archives.

## Finishing

End with the changed files, the verification you ran, the observed result, and
what remains unverified. Report any conflict between documentation and source
explicitly. Never commit credentials, a Home, or personal paths.
