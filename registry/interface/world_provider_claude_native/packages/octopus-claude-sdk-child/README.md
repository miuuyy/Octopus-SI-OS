# octopus-claude-sdk-child

The model-side child of `world_provider_claude_native`. It drives Claude Code
through Anthropic's Claude Agent SDK and speaks one private JSON-lines
protocol, `octopus.claude_sdk_child/1`, with the provider life that spawns it.
The frames are defined in [protocol.schema.json](protocol.schema.json), which
is generated from `src/protocol.ts` (`npm run schema`).

```sh
octopus-claude-sdk-child --claude-executable <command-or-absolute-path>
octopus-claude-sdk-child --doctor --claude-executable <command-or-absolute-path>
```

## Exchange

- The child prints `ready` once, then reads commands from stdin.
- Every command is answered by exactly one `ack` or one `error` with its
  `request_id`. A frame that is not valid protocol ends the child with a fatal
  `error` and exit status 2.
- `start` opens a session; `turn` sends one user message; the turn then emits
  `item` frames, a `tool_call` for each call to a granted tool, a `usage` once Claude
  Code finishes it, and ends with exactly one `turn_end`.
  One turn runs per session at a time.
- A `tool_call` keeps Claude's tool call open until the provider answers it
  with `tool_result` for the same `call_id`; the result text is what the model
  reads. Interrupting or closing answers every open call as an error.
- `interrupt` stops the running turn, which ends with `turn_end` status
  `interrupted`. `close` ends the session. A session that failed stays open
  until it is closed and rejects new turns.
- `turn_end` carries Claude Code's session id; passing it back as
  `config.resume` continues that conversation.

## Isolation

Claude Code keeps its own sign-in. The child stores no credential and reads
none; `--doctor` reports `claude auth status` without the account's email or
organization. On macOS, Claude Code keeps a claude.ai sign-in in the login
Keychain and reads it with `/usr/bin/security`, so the provider's `life.json`
grants that command and read access to `~/Library/Keychains`, as
`world_provider_claude_cli` does.

A session starts with every built-in Claude Code tool removed, no filesystem
settings, no MCP server but its own in-process `octopus` server carrying the
tools named in `start`, no plugins, no hooks, and verbatim prompts,
so prompt text is never expanded as a file mention or run as a slash command.
Claude Code is given an explicit environment, not the child's. When Claude Code
reports its startup capabilities, any tool, MCP server or plugin the session did
not grant fails the session with `tool_isolation_violated` before the turn is
answered.

## Build and test

```sh
npm ci
npm test
npm run typecheck
```

The build writes `bin/octopus-claude-sdk-child` and `build-receipt.json`; both
are generated and not committed.
