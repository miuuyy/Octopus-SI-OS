# Writing a tool package

A tool is an executable process behind the door interface of a worker
environment. The model sends an ordinary `tool.call` artifact; the door checks
the formed grant, loads `registry/tool/<id>/tool.json`, starts or reuses the
declared process, and hands its result artifacts back to the runtime. The
kernel does not decode the payload and does not link tool code.

Background: [Tools](../../docs/contracts/TOOL_REGISTRY.md).

## tool.json

```json
{
  "id": "example.read",
  "interface": "system_example_env",
  "process": {
    "bin": "target/debug/octopus-example-tool",
    "args": [],
    "required_files": [],
    "inherit_env": [],
    "inherit_env_from_interface": [],
    "authority": {
      "read_root_slots": ["example_root"],
      "write_root_slots": [],
      "executable_root_slots": [],
      "read_paths": [],
      "write_paths": [],
      "executable_paths": [],
      "executable_commands": [],
      "read_env_paths": [],
      "write_env_paths": [],
      "executable_env_paths": [],
      "allow_network": false,
      "allow_local_ipc": false
    },
    "coordinate_root_slots": []
  },
  "handler_config": {},
  "supported_worker_environment_kinds": ["example_kind"],
  "description": "Read from the granted example room.",
  "args_schema": {
    "type": "object",
    "additionalProperties": false
  },
  "is_world_effect": false
}
```

The folder and `id` match. `interface` is the door that owns the tool.
`handler_config` is opaque to everything except the package-local process. A
new tool over an existing process and operation is a descriptor only; a new
executable is package work inside the owning kind. There is no central handler
registry, no `runtime_handler_ref`, and no compiled mount table.
`supported_worker_environment_kinds` is required and contains exactly one
name: a tool belongs to one package and code-world, not to a list of
compatible rooms.

## Authority

The process starts with a cleared environment and receives:

- its own private writable var root;
- the interface descriptor and config;
- only the declared root slots, environment paths, executables, and network or
  IPC access;
- the invocation artifact and bound artifacts over the stdin protocol.

For a file tool whose path arrives in its arguments, use
`read_argument_paths`, `write_argument_paths`, or `executable_argument_paths`
with a JSON pointer and `within_root_slots`. A separate process is then created
with the exact authority of that one call; a path outside the permitted roots
is refused before execution. Do not give a whole process a writable room for
the sake of one file.

## The room's code-world

A tool process lives in the same package and code-world as the kind and door
of its worker environment. Each kind runs its own door life, built on the door
runtime that `process_session` owns (`octopus-process-session-tool-runtime`,
imported by the other kinds as `octopus-tool-door-runtime`); it reads the
ledger, checks the tool grant and formation, and talks to the package process.
Only artifacts cross the room's boundary.

The process receives a strict invoke JSON carrying the full tool descriptor and
the invocation envelope. It returns one or more artifact manifests, or a typed
`failed` or `unavailable`. A capability failure first becomes an admitted
failure artifact, and then the cursor is committed; `unavailable` or a crash
does not move the cursor.

## Invariants

- a tool descriptor requires no change to the compiled runtime;
- an unknown or malformed process or descriptor fails closed;
- the result is an artifact - not a Rust return value and not a mark;
- the process receives no ledger, no durable root, and no write authority over
  all runs;
- a fine-grained right is exercised by the door and process, which physically
  hold only the permitted handles.
