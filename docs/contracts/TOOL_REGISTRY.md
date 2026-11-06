# Tools

Tools sit **below worker environments**. A tool is code behind a room's door. A
tool call is an ordinary artifact emitted to the door interface; the room
executes it and returns a result artifact.

The kernel is blind to tools. It checks that the route is in the caller's
grant and passes the artifact exactly once. Fine-grained rights - "read-only in
this room" - are enforced by the room itself against the grant it receives in
the invocation envelope. Since the room is also the executor, that check
cannot be bypassed.

## The descriptor

```text
registry/tool/<tool-id>/tool.json
```

```json
{
  "id": "coder.read_file",
  "interface": "system_coder_env",
  "handler_config": { "operation": "read_file" },
  "process": {
    "bin": "target/debug/octopus-coder-fs-tool",
    "inherit_env": ["PATH"],
    "authority": {
      "read_root_slots": ["registry_worker_environment_kind_root", "repo_root"],
      "write_root_slots": ["git_common_root", "worktree_root"],
      "executable_root_slots": [],
      "read_paths": [], "write_paths": [], "executable_paths": [],
      "executable_commands": ["git"],
      "read_env_paths": [], "write_env_paths": [], "executable_env_paths": [],
      "allow_network": false,
      "allow_local_ipc": false
    }
  },
  "supported_worker_environment_kinds": ["git_worktree"],
  "description": "Read a bounded UTF-8 byte range from a text file inside the run's leased worktree.",
  "args_schema": { "type": "object" },
  "is_world_effect": false
}
```

- `id` - matches the folder.
- `interface` - the door that owns the tool.
- `process` - the tool's own executable and the authority it runs under: root
  slots, paths, environment, network.
- `handler_config` - opaque JSON for that executable. Together with `id`, it
  routes between operations inside one binary.
- `supported_worker_environment_kinds` - exactly one kind. A tool belongs to
  one code-world, not to a list of compatible rooms. The reverse edge is
  ownership: the tool's binary must be one the kind declares as its own.
- `args_schema` - parsed and enforced by the door's code, not by the kernel.
- `is_world_effect` - whether the tool crosses a World boundary.
- `copy_of` - on a private copy, the tool it was copied from. An environment
  that allows the original admits the copy only while it is never wider: the
  same executable, handler and schema, under an authority the original's
  covers. A block on the original always reaches the copy.

Adding an operation over an existing binary is a JSON edit. Adding a new
executable is package work inside the owning kind, outside any runtime decision
path.

## Invocation

1. **Emit.** The model, or any driver, emits a tool-call artifact toward the
   room's door. One API response may carry many tool calls; the provider
   interface emits many artifacts and each crosses independently.
2. **Gate.** The kernel checks the route against the caller's grant, records
   the movement, and passes it once. It does not decode the call.
3. **Execute.** The door parses its own payload, checks the requested tool
   against the grant in the invocation envelope, leases the room, and runs the
   tool's declared process under its declared authority.
4. **Return.** The result comes back as an ordinary artifact movement.

A tool does not emit a mark. A build result artifact can be the *candidate* for
a checkpoint's required verified artifact, but the tool never claims the mark
itself. See [Marks and verifiers](VERIFIERS.md).

## Authority is physical

A tool process starts with a cleared environment and receives only:

- its own private writable state directory;
- the door interface's descriptor and config;
- the root slots, environment paths, executables, and network or IPC access its
  descriptor declares;
- the invocation artifact and bound artifacts over stdin.

A tool whose path comes in its arguments declares that argument as a path within
specific root slots. The process is then created with exact authority for that
one call; a path outside the allowed roots is refused before anything runs. A
whole writable room is never handed out for the sake of one file. A path
argument resolves only against the root slots its own declaration names, in no
particular order; a candidate slot that cannot be resolved never refuses a path
inside another one, and a refusal names it. A coordinate the process needs
without access to it, such as the installation root a packaged MCP program is
found under, is declared in `coordinate_root_slots`; no other slot reaches the
process.

Physical isolation uses the default enforced sandbox mode. An explicit
`sandbox_mode: "disabled"` opts out of the OS sandbox; policy checks alone do
not provide that isolation. See [Security model](../security-model.md).

The tool binary is blind to the runtime, the ledger, policy, and marks.

## Fail closed

A malformed descriptor, a missing binary, bad handler config, schema-invalid
arguments, a missing interface binding, or a duplicate registration all fail
closed. Nothing falls back to a product-specific path or a hidden default, and
the runtime never installs a placeholder success.

## Private process transport

Inside the process-session code-world, the tool control frame carries a checked
reference to its invocation context. The complete grant metadata and bound
artifact catalogue are written into a private file for that exact process
generation and request. This is transport storage, not an artifact published to
the runtime or model context. No authority or provenance is omitted to fit the
control frame.

The receiver checks the owned path, declared byte limit, exact length and content
hash before decoding. Symlinks, cross-request references and changed bytes fail
closed. Input retirement pins this file with the primary request and removes it
only after the outcome is retained. Package updates replace both protocol peers;
accepted output recovery retains its existing invocation and result evidence.

A failed tool process result names its failure class and the first line of
its diagnostic in the error message; the bounded details keep the full
evidence.

## Persistent commands and recovery

A persistent command belongs to its formed worker environment. Its keeper and
retained call results are reached through that environment's durable state,
independently of the process serving a tool call. Tools behind the same door can
use different process authority or recovery declarations while continuing,
listing, signalling, or releasing the same commands. Their executable and
handler operation must still belong to the declared worker-environment kind.

Workspace command tools declare their output wait limits in
`handler_config.max_yield_time_ms`. A longer requested wait is capped to the
configured window instead of rejecting the command: 30,000 ms for
`workspace.exec` and 300,000 ms for `workspace.write_stdin`. Both return early
when the command exits. The process deadline is separate; subsequent reads
continue the same process. Descriptor changes take effect without restarting
the runtime or replaying commands.

`process.interrupted_effect_recovery` controls whether an invocation whose
outcome is unknown may be executed again. `reinvoke_exact_artifact` requires the
tool to recover that exact call safely. An omitted value is `indeterminate`:
the door reports the uncertain outcome instead of repeating the effect. A
retained completed output is replayed without executing the tool again.

While the original fan-out of a retained output has pending edges, or its
current source is still settling, the door hands in the same artifact under the
same delivery identity again, after each ledger change or a short interval,
until the delivery settles or the door is cancelled. A changed scope or
conflicting evidence fails closed and leaves the output retained for recovery.
Streamed outputs, failure results and environment release results hand in
again the same way while their refusal only awaits the current source; every
other refusal stays terminal.

## What ships

The registry carries about 190 tool descriptors over kind-owned executables:

| Family | Room | Examples |
| --- | --- | --- |
| `coder.*` | `git_worktree` | `read_file`, `apply_patch`, `grep`, `build`, `exec`, worktree diff and snapshot |
| `workspace.*` | `filesystem_workspace` | read, write, search, and run inside a real project folder |
| `browser.*` | `browser_session` | navigate, click, type, snapshot, screenshot, downloads, tabs |
| `gui.*` | `desktop_session` | screen capture and input |
| `process.*`, `exec.*` | `process_session` | persistent processes |
| `container.*`, `remote.*` | `container_session`, `remote_exec_server` | execution elsewhere |
| `mcp.*` | `mcp_stdio_session` | tools, resources, prompts of an MCP server |
| `code_mode.*` | `code_mode` | run, wait, resume |
| `file.*`, `artifact.*`, `skill.*` | `system_room` | the system's own files, artifact access, skills |
| `web.*`, `microphone.*`, `speaker.*` | `web_search`, `audio_device_session` | search and audio |

The authoring guide is [registry/tool/GUIDE.md](../../registry/tool/GUIDE.md).
