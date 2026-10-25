# pre_tool_use_guard

This verifier checks a `tool.call` artifact before it reaches a worker-environment
door. Its role is similar to a `PreToolUse` hook: the checkpoint names the
verifier in `required_verified_artifacts`, and a passing verdict admits the
configured mark. A failing verdict records `VerificationFailed` and leaves that
checkpoint open.

The package-local `verify.py` reads the artifact payload and accepts these
checkpoint parameters:

- `denied_tools`: exact tool ids to reject.
- `denied_command_substrings`: case-insensitive substrings to reject in the
  payload's command or argument fields.
- `allowed_commands`: bare program names that may run. When set, a call that
  carries a command (`command` or `cmd`, at the top level or under
  `arguments`/`args`) passes only if it runs one listed program. A command
  string must be a single plain command: any shell metacharacter (`;`, `|`,
  `&`, `# pre_tool_use_guard

This verifier checks a `tool.call` artifact before it reaches a worker-environment
door. Its role is similar to a `PreToolUse` hook: the checkpoint names the
verifier in `required_verified_artifacts`, and a passing verdict admits the
configured mark. A failing verdict records `VerificationFailed` and leaves that
checkpoint open.

The package-local `verify.py` reads the artifact payload and accepts these
checkpoint parameters:

- `denied_tools`: exact tool ids to reject.
, backticks, quotes, backslash, redirection, globs, newlines) is
  refused, and its first word must exactly match a listed name. An argv array
  runs without a shell, so only its first element is checked, again by exact
  name. A path such as `/bin/ls` never matches. An empty or malformed command
  is refused. A call with no command field is left to the other checks.

### Choosing between the two lists

`denied_command_substrings` matches text, not what the command does. It cannot
see `r''m`, `rm` split by a backslash-newline, a base64 string piped into `sh`,
or a different program with the same effect (`unlink`, `find -delete`, `head`
for `cat`), and a short substring also blocks harmless commands: `rm` matches
`npm run format`, and `dd` matches `cargo add`. Measured with
[gate-bypass-bench](https://github.com/zbs-ops/gate-bypass-bench) against this
verifier's own `verify.py`, denying `rm`, `cat`, `dd`, `curl`, `wget` and
`chmod` let 14 of 45 disguised forbidden commands through. The same 45 against
`allowed_commands: ["echo", "ls", "true", "pwd"]` let none through.

Use `allowed_commands` when a checkpoint should admit only known programs, and
keep `denied_command_substrings` for blunt extra blocks such as `--force`.
Like every check here, both are a policy layer, not process isolation; the
sandbox still decides what a process can physically touch.

It rejects an unreadable payload, an invalid JSON object, an artifact whose label
is not `tool.call`, and a missing tool id. It does not execute the tool or provide
a general hook lifecycle. Non-blocking observation belongs to interface code.

`registry/mark/hook.gate.passed/mark.json` declares the success mark for this
check. Allow it in the worker pin's `ceiling.mark_outcomes` and the EE policy,
then name it in the checkpoint's requirement.
