# Marks and verifiers

Checkpoints can require evidence before work continues. A verifier checks the
evidence artifact, and the runtime records a mark when the check passes. That
mark establishes the specific result checked by the verifier.

Three words, three different things:

- A **required verified artifact** is a *checkpoint requirement* authored in the
  run record: this checkpoint closes only when an artifact with this label
  arrives and this verifier passes it.
- A **verifier** is a deterministic check that runs *on the artifact edge*.
  Picture two interfaces as two points with artifacts travelling the line
  between them. The verifier stands in the middle of that line deciding whether
  the artifact fits what the far side expects.
- A **mark** is the *success tick*: the ledger fact that a required verified
  artifact was closed by artifact Y through verifier Z. Nothing emits marks  - 
  not the model, not the interface. The runtime records one after the verifier
  gate passes. There is no mark-emission tool or operation anywhere.

A failed verdict never becomes a mark. The artifact still *reached* the
checkpoint, so the arrival and the failure are durable ledger rows  - 
`InterfaceArrival` and `VerificationFailed` with machine-readable details. The
tick simply does not appear, nothing the checkpoint would have done next is
done, and the interface that sent the artifact decides for itself whether to
author a retry.

## 1. The checkpoint contract

Requirements are declared in exactly one place - the checkpoint in the run
record:

```json
{
  "id": "c1",
  "run": "run_web",
  "epoch": 0,
  "interface_binding": { "interface": "world_provider_codex" },
  "required_verified_artifacts": [
    {
      "mark": "ocr.text.contains",
      "artifact_label": "screen.captured",
      "verifier": "ocr_text_contains",
      "params": { "expected_text": "OCTOPUS" }
    }
  ]
}
```

- `params` are plain JSON authored with the run: the variables of the check.
  The runtime does not understand or validate them. Their schema is entirely
  the verifier's responsibility.
- `verifier` is required. A blank check is still an explicit package - the
  `artifact_exists` verifier, "the artifact arrived and is readable" - never a
  hidden label-only pass inside the runtime. This is how a model can discipline
  its own long-running work with future obligations even when nothing semantic
  is checked.
- Requirements are per checkpoint, not per run. Two requirements with the same
  `artifact_label` on one checkpoint are ambiguous and fail closed: one edge
  cannot close several requirements. To close several at once, an interface
  sends several artifacts.
- A checkpoint *with* requirements takes no edge for an artifact whose label
  matches none of them: the fan-out skips it, the same way it skips a binding
  that does not admit the name. A checkpoint *without* requirements admits
  ordinary arrivals, each distinct artifact once. The axis is optional; most
  checkpoints do not use it.

## 2. Admission on the edge

For each arrival, the runtime:

1. checks the formed grant - every authored `mark` id must be inside the run's
   granted `mark_outcomes`; an unauthorized mark id is a policy failure;
2. matches the artifact's label against the checkpoint's requirements;
3. loads the mark and verifier descriptors from the registry, merges the
   verifier's `default_params` under the authored `params`, and runs it;
4. on **pass** - records the mark as a `MarkOutcomeAdmitted` movement, the tick
   other runs can wait on;
5. on **fail** - records `VerificationFailed` with the verdict's details,
   returns the error to the interface, and does nothing else.

The runtime never parses the payload and never learns what the requirement
means. The meaning of a model API error, OCR quality, or a finished plan item
lives in the verifier, not in the kernel.

## 3. The verifier contract

**Input:** the artifact, the movement metadata, and the merged params.
**Output:** a strict verdict, passed or failed, plus machine-readable details.

A verifier does not wake the runtime, does not create checkpoints, does not
edit run records, does not write marks, and is not an agent. It must not pass
silently on any malformed leg: a missing blob, an unreadable payload, a hash
mismatch, a missing engine, a declared time limit that ran out - all `failed`,
with details. A verifier process runs until its verdict, its death or
cancellation; only a package that reaches outside the system declares
`timeout_ms`, and `params` may only narrow it. A slow machine is not a failed
check.

## 4. Verifiers are removable packages

```text
registry/verifier/<id>/verifier.json
registry/verifier/<id>/<script>
```

```json
{
  "id": "artifact_exists",
  "package": "artifact_exists",
  "script": "verify.py",
  "authority": {
    "read_root_slots": [], "write_root_slots": [], "executable_root_slots": [],
    "read_paths": [], "write_paths": [], "executable_paths": [],
    "executable_commands": ["/usr/bin/env", "python3"],
    "read_env_paths": [], "write_env_paths": [], "executable_env_paths": [],
    "allow_network": false, "allow_local_ipc": false
  },
  "default_params": {}
}
```

The script reads one request JSON on stdin and prints one verdict on stdout:

```json
{"passed": true, "details": {}}
```

- **Any language.** Shell, Python, JavaScript, or a compiled binary named in
  the same `script` field.
- **Sandboxed by default.** The process gets its package directory and a scratch copy of
  the payload. It gets no ledger, no registry, no network unless declared.
  An explicit disabled sandbox mode opts out of OS isolation; see
  [Security model](../security-model.md).
- **The command never comes from params.** The code is chosen by whoever
  installs the package. A run author cannot pass a command in through data.
- **Loaded per admission.** Adding or removing a package needs no restart.
- **Removable.** Deleting a package breaks, fail-closed, only the checkpoints
  that reference it. An unreferenced missing verifier is a non-event.

The runtime's verifier host is a blind script runner. It knows no verifier's
identity and no language.

Non-JSON output, a non-zero exit, a missing `passed`, or a declared time limit
that ran out all count as failed.

## 5. Marks are vocabulary plus a grant axis

```text
registry/mark/<id>/mark.json
```

```json
{ "id": "coder.build.ok", "package": "coder.build.ok", "class": "success" }
```

A mark descriptor is vocabulary only. It names an outcome; it carries no
verifier, no evidence rule, and nothing executable. The verifier is chosen on
the checkpoint. The evidence is the artifact that closed the requirement.

A worker pin allows outcomes through `ceiling.mark_outcomes`, and only a run
whose formed grant contains a mark id may have checkpoints that close it.

Mark names are registry vocabulary, not a fixed contract. What is
architecturally load-bearing is the shape - checkpoint-authored requirements
gated by deterministic verifiers over artifact evidence - not any particular
name.

## 5b. Filtering a route with a verifier

Verifiers stand on concrete checkpoints, not on bindings.

"The operator always receives the reply; the tool room receives it only if it
is a well-formed tool call" is expressed as two checkpoints fed by separate
artifacts from one model reply: the client-bound checkpoint with no
requirements, and the room-bound checkpoint with a format verifier. The reply
text never reaches the room-bound checkpoint, because no requirement there
names it. A tool call that fails the check is a `VerificationFailed` row on
that route only. The provider write that carries a tool call arms the
one-shot wait for its result with the call already in that wait's context.
Every provider write keeps each result door armed: the doors a run's active
checkpoints pin are what keep their tools in the formed grant. The client
branch does not suffer.

## 6. Coordination between runs

A checkpoint that depends on another run's work and must be proven waits on
a mark. The producer's result artifact arrives on the waiter's binding, the
verifier runs on that edge, and the admitted tick wakes the waiting run. Use
this where proof makes the result better; an ordinary dependency is simply an
awaited artifact.

Nothing coordinates on failures. Runs wait only on admitted ticks; a
`VerificationFailed` row is diagnostic evidence, not a wait key.

An `InterfaceArrival` carrying `verification_failure` is finished rejection
evidence, not a candidate for mark proof. Restart replay retains these rows
without treating repeated rejected attempts as ambiguous pending arrivals.
A rejected arrival cannot replace a pending arrival: a `replaces_arrival_seq`
claim on it still fails replay. Ordinary indistinguishable pending arrivals
without a valid replacement link remain an error.

No module routes marks. Delivery across environments *is* the artifact-transfer
protocol.

## What a mark does not prove

A mark proves that its verifier passed over its evidence artifact. It does not
prove arbitrary correctness or safety. `coder.build.ok` means the build
verifier accepted a build artifact. Marks that detect deviation - proof a
worker stayed on-contract - are still to be designed.

Authoring guides: [registry/verifier/GUIDE.md](../../registry/verifier/GUIDE.md),
[registry/mark/GUIDE.md](../../registry/mark/GUIDE.md).
