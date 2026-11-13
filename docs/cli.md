# Terminal client

Octopus CLI is another client of the same system. It uses the same Home,
Projects, Agents, tasks, model settings, and recorded results as the desktop
client. You can start a task in either client and continue it in the other.
The terminal does not need Computer Use to send work or read its results.

## Build and open

The current distribution targets macOS 14 or later. Use Node.js 24 or newer,
Rust, and the Xcode Command Line Tools listed in
[Getting started](getting-started.md#prerequisites). From the checkout:

```sh
npm ci --prefix app
npm --prefix app run build:cli
./octopus
```

This builds the terminal modules and release runtime/capability distribution
under `app/build/cli`. It does not build or open the desktop app. For a terminal
presentation change, `npm --prefix app run build:cli-client` rebuilds only the
shared client modules. If Node 24 is not on your `PATH`, set `OCTOPUS_NODE` to
its executable when launching `./octopus`.

To use an existing desktop Home, select that exact directory:

```sh
./octopus --home "$HOME/.octopus"
```

The default follows the desktop convention: `OCTOPUS_HOME`, or `~/.octopus`,
with that Home's `.env` file. An explicit `--home` pins the selection. There is
no separate terminal task store. The runtime is shared; exiting the CLI leaves
it and admitted work running.

An empty Home opens setup. Choose an application pack, workspace name, optional
first Project, model, and reasoning level. Existing machine provider sign-in
can be reused; otherwise setup prints the native sign-in address to open in
your browser. Once configured, normal startup opens Home. Setup can also be
started explicitly with `./octopus setup`. An explicit `--service-tier <id>`
selects a tier from the model catalogue and preserves it in Home and setup
Agent presets.

Source changes do not automatically update packages already installed in a
Home. Use matching built client/package versions. Package proof errors must
be repaired at the named owner; the terminal does not reset configured Homes
or silently replace installed files.

## Working in the terminal

Write a message and press Enter. Use Alt+Enter for a new line; pasted multiline
text remains a draft. Replies, tool activity, and questions appear in the
terminal's normal scrollback. Arrow keys navigate pickers and input history.
Type `/` to browse commands; `/help` shows the keyboard controls.

| Command | Result |
| --- | --- |
| `/home` | Continue the ongoing Home conversation. |
| `/new` | Start a task in a selected Project with one of its Agents. |
| `/project`, `/agent` | Choose or create a Project or Agent. |
| `/model` | Choose a permitted model and its reasoning level. |
| `/tasks`, `/resume` | Find and continue existing tasks. |
| `/attach <path>` | Attach a file or folder to the next message. Quote paths with spaces. |
| `/files`, `/remove` | Review or remove pending attachments. |
| `/answer` | Answer the task's current question or permission request. |
| `/stop` or Esc during work | Request Stop through the task's existing interface. |
| `/history`, `/search <text>` | Read or search recorded work. |
| `/rename`, `/archive`, `/archived`, `/restore` | Organize tasks. |
| `/delete` | Confirm deletion of a settled task; recorded evidence remains. |
| `/detach` | Exit while work continues. |

Model discovery uses the configured provider. Native Codex models come from
the installed CLI's catalogue; an OpenCodex provider uses its own declared
catalogue. Provider and Project policy still limit selection. An unavailable
model is an error, not permission to select another one automatically.

An Agent's configured Applications and Drivers supply its task capabilities.
Attachments use the same admitted artifact path as desktop attachments;
attaching a path does not grant arbitrary access to the surrounding filesystem.
Questions and permissions preserve the actual offered choices and scopes.
Closing a question with Esc sends no response; `/answer` opens it again.
After Stop, the client waits for the recorded result of any tool already in
progress before accepting the next message. A Stop acknowledgement alone does
not confirm that an external effect has ended.

Browser tools require a running, declared browser host. The CLI distribution
includes their capability packages but does not start the desktop Electron
browser host. Sending tasks, reading results, and using other configured tools
does not require that host.

## Commands for scripts and other agents

Read commands return JSON with `--json`. Task commands stream newline-delimited
JSON when waiting, including the admission receipt, activity, and final result.
They use the same user-facing operations as interactive chat.

```sh
./octopus up
./octopus projects --json
./octopus project create "Research" --json
./octopus project folders <project-id> ~/work/research --json
./octopus models --agent <agent-id> --json
./octopus exec --agent <agent-id> --json "Summarize the attached notes" \
  --attach ./notes.txt
./octopus send <task-id> --json "Expand the second point"
./octopus watch <task-id> --json
./octopus history <task-id> --json
./octopus inspect <run-id> --json
./octopus stop <task-id>
```

`exec --access project_access|full_access` chooses the new task's access; the
default comes from the Project and is full access unless the Project sets
project access. `agent create --application <id>` (repeatable)
connects Applications to a new Agent. `project delete <id> --yes` and
`agent delete <id> --yes` remove a Project or an Agent through the same
operations as the desktop client.

Exact IDs, unique ID prefixes, or unambiguous names select existing tasks.
`resume last` and `resume home` are available. For automation, prefer exact IDs.
Use `exec -` or `send <task-id> -` to read a prompt from standard input.
`--no-wait` returns once the input is admitted, without claiming completion.
`--timeout 60` detaches after a minute; it does not stop the task.
The `stop` command returns the control request's admission receipt. For an
active task, keep `watch` running to wait for the recorded Stop result.

`answer <task-id> <request-id> --answer '<json>'` submits a structured response.
Simple approvals can use `--decision accept`, `decline`, or `cancel`; requests
offering persistence also require their exact `selected_persistence` in JSON.
Use the `requestId` and request data emitted by `watch --json`, or answer interactively.

`export <task-id> <file>` saves the transcript. `download <task-id>
<attachment-id> <file>` retrieves an admitted attachment and verifies its
content. Both require a new destination file. `--output-last-message <file>`
saves the final reply after a waited task completes.

`delete <task-id> --yes` removes a settled task and its private active records.
The ledger and blob evidence remain available. Home cannot be deleted.

`inspect <run-id>` shows checkpoint requirements, their recorded evidence, and
outbound transfers matching current interface bindings. It also accepts `home`
and `last`. This is a read-only view: the runtime still decides admission and
whether a repaired binding can catch up. JSON output includes movement and
artifact references, coverage, and any uncertainty in the projection.

| Exit code | Meaning |
| --- | --- |
| `0` | Command succeeded; waited work has a recorded completion. With `--no-wait`, only admission is confirmed. |
| `1` | Invalid request, failed operation, connection failure, or recorded task failure. |
| `3` | Work requires an operator response. |
| `124` | Waiting timed out; inspect or resume the task. |
| `130` | Work has a recorded stopped result, or Ctrl+C detached the noninteractive client. |

`status --json` reports runtime and client readiness separately. An empty Home
can have a running runtime before setup creates its client interface. Runtime
startup logs are under the selected Home's `client/logs/runtime-terminal.log`.

The first terminal version omits voice, gesture controls, scheduled-task
editing, graphical widgets, and extension downloads. Those configured
capabilities remain part of the same Octopus installation.
