# Security model

Octopus runs a model with tools on your machine. This page says what stands
between a model's output and an effect, and what does not.

## Trust boundaries

| Party | Trust |
| --- | --- |
| **You, the operator** | Trusted. You grant projects, capabilities, and applications. |
| **The model** | Not trusted. Its output is a request, never an authority. Anything it read from the world can steer it. |
| **Artifacts from World** | Not trusted. A web page, a message, a file, or an MCP result may carry instructions aimed at the model. |
| **Installed interfaces, rooms, tools, verifiers** | Trusted the way an installed app is: you chose the code. Process isolation depends on its declared authority and sandbox mode. |
| **The runtime** | Enforces routes and grants. It never reads a payload and never takes a model's word for anything. |

## Two independent layers

**Policy decides what a run may ask for.** A run's grant is formed at every
arrival from its worker's ceiling, the chain of environments it lives in, and
its authored bindings. A request outside that grant is refused at the gate and
recorded as a `DeliveryRefused` movement. Nothing a model writes into a run
record becomes a right, because the grant is recomputed from descriptors, not
read from the record. See [Policy](contracts/POLICY.md).

**Process authority decides what a process can physically touch.** Tools,
verifiers, and interface processes start with a cleared environment. With the
default `sandbox_mode: "enforced"`, they run inside a deny-by-default macOS
sandbox (`sandbox-exec`). Each receives only the root
slots, paths, executables, environment names, and network or local-IPC access
its descriptor declares. A command declared by name is found through the
declared `PATH` and runs only from its canonical file; the name it was found
under and each symbolic link on the way get metadata access only, so the
process can call it by that name without reading what sits around it. The
launching code writes that sandbox profile to a
private file, and every profile denies writes to the pending profile files in
its location, so a process with broad write authority still cannot rewrite
them. A file tool whose path arrives in its arguments is
started with authority for that one path; a path outside the permitted roots is
refused before anything runs. A descriptor can explicitly select
`sandbox_mode: "disabled"`; that process then runs without this OS sandbox.
Do not treat a policy grant as equivalent to physical process isolation.

The two layers do not depend on each other. A room enforces the operation-level
grant itself, and since the room is also the executor, a model cannot talk its
way past it.

In enforced mode, a platform with no backend that can enforce independent read,
write, and execute roots refuses to start the process. The preview targets macOS.

## What is recorded

Every boundary crossing is a row in `ledger/movements.jsonl`, including
refusals and failed verifications. The log is append-only. It is your audit
trail: what arrived, where it went, what was refused and why.

A mark is recorded only after a verifier passes an artifact. A model saying it
finished is not a mark.

## Principal risks

### Prompt injection and destructive tool use

A run that reads untrusted content and also holds write or execute tools can be
steered into using them. The architecture narrows the blast radius; it does not
remove the risk.

- Give a project the smallest set of drivers and applications it needs.
- Prefer a `git_worktree` room when you want changes isolated on a branch.
- A room's boundary can only be narrowed by policy, never widened.
- Checkpoints can require a verified artifact before they close. The registry
  ships verifiers for operator approval and for guarding tool calls.

### Self-modification

A run whose room covers a registry path can change the system's own
descriptors: add an interface, a worker, a tool. That is the point of the
design, and it is gated like everything else - by the policy of the environment
the run lives in. Do not grant registry paths to environments that also read
untrusted input unless you mean to.

### Credentials

- Provider sign-in is held by Codex's own authentication, normally under
  `~/.codex`. It is not contained in the Octopus Home. Octopus's Codex
  provider, the managed Codex app-server and your own Codex CLI share it, and
  every token refresh and sign-out takes the store's `auth.lock`, the lock
  Codex itself uses, so they never rotate the refresh token over each other.
- An interface's secrets stay in that interface's private state. Descriptors
  carry the *name* of an environment variable or a handle, never a value.
- Never paste a Home, a ledger, or an interface state directory into an issue.

### Same-user local processes

A Home is a directory of files owned by your user account, and by default the
client reaches the runtime through Unix sockets inside it. Anything running as
your user can read that directory. The client's local password, stored as a
scrypt verifier, and the optional Touch ID unlock protect the app's window, not
the files underneath. If another person or an untrusted process can act as your
user, treat the Home and the provider session as compromised.

### Installed packages

An interface is an app. Installing it admits someone's code into the active
registry; live bindings make its processes run with the authority its
`life.json` declares. Read that file before installing a package you did not
write, including its sandbox mode. In enforced mode the sandbox limits a
package to what it declares; it does not make a malicious package safe.

### What a mark does not prove

A mark proves its verifier passed over its evidence artifact. "It builds" is a
check. Proof that a worker stayed on-contract and wrote nothing harmful is not
designed yet.

## Non-goals

- Octopus is not a multi-tenant security boundary. One Home, one operator.
- A model capable enough to route around its own policy is acknowledged, not
  solved. Policy and marks are a control surface, not a proof of safety.
- A replayable ledger does not make provider responses or external effects
  deterministic.
