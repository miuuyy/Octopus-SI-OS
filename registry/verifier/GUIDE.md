# Writing a verifier package

A verifier is a deterministic check of one artifact on a movement edge: an
artifact arrives at a checkpoint, the checkpoint has
`required_verified_artifacts`, the runtime calls the verifier, and the verifier
returns a verdict. Pass, and the runtime writes a mark - the success tick.
Fail, and the ledger gets a `VerificationFailed` movement (the artifact
*arrived*; the tick did not appear), the checkpoint stays open, and the
interface receives the error with details.

A verifier is **not** an agent. It does not wake the runtime, create
checkpoints, change run JSON, or write marks itself.

Background: [Marks and verifiers](../../docs/contracts/VERIFIERS.md).

## Where it goes

```text
registry/verifier/<id>/verifier.json     the descriptor (required)
registry/verifier/<id>/<script>          the executable check
```

The package is removable. Delete the folder and only the checkpoint or run
JSON that references it breaks, fail-closed. An unreferenced missing verifier
is a non-event.

## verifier.json

```json
{
  "id": "my_check",
  "package": "my_check",
  "script": "verify.sh",
  "authority": {
    "read_root_slots": [],
    "write_root_slots": [],
    "executable_root_slots": [],
    "read_paths": [],
    "write_paths": [],
    "executable_paths": [],
    "executable_commands": ["/bin/sh"],
    "read_env_paths": [],
    "write_env_paths": [],
    "executable_env_paths": [],
    "allow_network": false,
    "allow_local_ipc": false
  },
  "default_params": {}
}
```

- `id` equals the folder name, or loading fails closed. `package` equals `id`.
- `script` is a file name strictly inside the package - no `/`, `\`, or `..`.
  The code is chosen by whoever installs the package. A command **never** comes
  from params.
- `authority` is the process's physical authority. The package directory and a
  separate scratch directory with the payload are granted automatically; the
  ledger, the registry, the durable root, and the network are not. The
  interpreter or executable is listed explicitly. The process starts with a
  cleared environment in a fail-closed sandbox.
- `default_params` is a JSON object merged under the `params` from run JSON.

## How run JSON references a verifier

```json
"required_verified_artifacts": [{
  "mark": "ocr.text.contains",
  "artifact_label": "screen.captured",
  "verifier": "ocr_text_contains",
  "params": {"expected_text": "OCTOPUS"}
}]
```

`mark` is an id from `registry/mark/`. `artifact_label` says which artifact is
the candidate. `verifier` is an id from `registry/verifier/`. `params` are
ordinary JSON authored with the run - the variables of the check, such as
`expected_text` or `lang`. The verifier validates its own params. The runtime
also reads the optional `params.timeout_ms` process limit described below.

## The executable contract

The script is executable (it has a shebang) and receives one JSON on stdin:

```json
{
  "run": "...", "checkpoint": "...", "epoch": 0, "mark": "...",
  "artifact": {"id": "...", "content": "<hex>"},
  "artifact_label": "...",
  "params": {},
  "metadata": {},
  "payload_path": "/tmp/...",
  "payload_utf8": "..."
}
```

`params` is already merged. `metadata` is the movement and invocation
metadata. `payload_path` holds the payload bytes in a temporary file;
`payload_utf8` is the text, or `null` when the payload is not UTF-8.

It prints exactly one JSON verdict on stdout:

```json
{"passed": true, "details": {}}
```

Non-JSON stdout, a non-zero exit, a missing or non-boolean `passed` field,
or exceeding a configured time limit all fail closed as `failed`.

There is no implicit timeout or fixed maximum. A package that reaches outside
the system may set `timeout_ms` in its descriptor. An unsigned integer
`timeout_ms` in the merged params can supply a limit on its own or narrow the
descriptor's limit; when both are set, the runtime uses the smaller value.
The effective limit is at least 1 ms. With neither configured, the process
runs until it returns a verdict, exits, or is cancelled.

A Rust verifier is built as its own executable and named in the same `script`
field. Python, shell, and JavaScript use the same stdin/stdout contract. A new
verifier changes neither the runtime, nor app bootstrap, nor any compiled
dispatch table.
