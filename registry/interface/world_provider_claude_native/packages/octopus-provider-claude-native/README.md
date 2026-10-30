# octopus-provider-claude-native

The provider life of `world_provider_claude_native`. It reads its own
movements, runs each declared input as a Claude Code turn through the SDK
child in `../octopus-claude-sdk-child`, exposes the invocation's admitted
tools to Claude, and hands replies, tool calls and failures through
`octopus-run-authoring`.

## Which Claude Code it runs

`private_config.sdk_child.claude_executable` names the Claude Code executable.
The default, `claude`, is looked up on the runtime's `PATH` and is granted by
`authority.executable_commands` in `life.json`. To use a specific install,
set it to an absolute path and add exactly that path to
`authority.executable_paths`. Both fields are declared package configuration,
so they survive package repair. The doctor and the life refuse to start
while the configured executable is not granted.

Claude Code keeps its own sign-in. On macOS a claude.ai sign-in is read from
the login Keychain, which `life.json` grants through `/usr/bin/security` and
read access to `~/Library/Keychains`.

## Model and reasoning effort

`private_config.model` and `private_config.reasoning_effort` are what the
client's model picker rewrites in a Home's private copy of this interface. The
life reads its descriptor from the registry before every turn, so a change
applies to the next turn: the Claude session is closed and resumed with the
new model or effort, keeping the conversation. A changed `sdk_child` is not
adopted that way; the turn fails with `descriptor_changed` until the life
restarts and verifies the new child.

## Inputs, replies and streaming

The life accepts the labels declared in `private_config.input_messages`,
decoded by the same contract the Codex provider reads. A `realtime.delegation`
from a voice call is an ordinary Home turn: its whole payload, including
`call_id`, `correlation_id` and `delegation_id`, reaches Claude unchanged in one
`input_message_context` block. Every reply and turn error carries `in_reply_to`,
the admitted input it answers, which is how the client attributes an answer to
its voice delegation.

Claude's text streams while the turn runs as `agent.message.delta` events in an
open `turn.stream` under the interface's `var/streams/`, in the layout the
operator chat client tails. The sealed stream is handed just before the reply,
whose item id is the streamed message it completes.

## Controls and restarts

While Claude runs, the life reads the ledger through a second cursor for the
run's controls. A stop or interrupt ends the Claude turn and is acknowledged
as `turn.control.acknowledged`; the turn then ends with the text streamed so
far, as a reply whose lifecycle is `interrupted`. A new message for the same
run redirects the turn: Claude is interrupted and the message runs as the next
Claude turn in the same session, acknowledged as a steer and recorded in the
turn stream when Claude takes it up. A control that arrives while the run is
waiting on an Octopus tool abandons that call; its result is later closed as
`turn.interrupted.result`.

The journal records the turn in flight and any tool call it handed. After a
restart, a result for such a call resumes the run's Claude session and reaches
Claude as a message; a source cut mid-answer is replayed by asking Claude to
answer it again in full.
