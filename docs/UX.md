# UX

Octopus has to feel native - like an operating system, not like a research
harness with a chat window attached. That is the main difference from other
self-improving agent projects: the architecture exists so that the experience
can be simple. Roughly three quarters of the experience follows Codex; one
quarter is the operating system.

This page records the decisions behind that experience. The screens themselves
are described in the [desktop client guide](client.md).

## The client sets the UX

The client decides what the experience is. Each piece of it is then served
where it belongs:

| Where | What it carries | Example |
| --- | --- | --- |
| The client | Presentation, local input, setup actions, the client's own composition. | Windows and panes, dictation, gestures, onboarding. |
| JSON | Everything prepared in advance. | A setup preset, an Agent template, the bindings of a new task. |
| The provider | How a turn is steered and handled inside the Codex provider. | Steering a running turn, approvals, the model catalog. |
| The architecture | Only what cannot be correct anywhere else. | Several writers re-arming one run at once. |

The last row is the rarest. When concurrent writes to one run record broke
tasks, the fix was architectural: checkpoint generations and ordered writes
under exact path authority. That is the kind of UX problem that belongs in the
kernel. Almost everything else does not.

A useful test: does this action reach a model, a run, or a world another
interface owns? Then it crosses an interface under policy. If it is the
client's own presentation or preparation, it stays in the client and is not
routed through runtime policy for the sake of consistency.

## Built around a ChatGPT subscription

Octopus is universal, but its experience is built on a ChatGPT subscription and
on Codex. The Codex provider is the main provider. It keeps its place in the
hierarchy - one interface, never the kernel - but the product is shaped around
it.

Other models enter as models inside the Codex provider, not as separate agent
products. [OpenCodex](https://github.com/lidge-jun/opencodex) is the example:
it lets Codex run models from many providers. In the registry each such
provider is its own interface folder that reuses the Codex provider's OpenCodex
driver, generated from one list so every provider follows the same OpenCodex
release. This is a quiet center of the UX: Codex stays the way you work, and
the set of models behind it can grow.

Earlier integrations borrowed from other agent projects are kept where they
are cheap to maintain and dropped where they are not: Slack stays as a
connector, and the other messaging channels and unconfigurable service stubs
were removed.

## Where to go

**Home is the main screen.** It is the cheapest way into the system: the last
reply, a composer, voice mode, and widgets. Think of it as the user's control
panel. Home has no model picker and asks nothing technical of you. It hands
out the work: when something needs doing, Home creates the runs that appear as
Tasks.

The nearest parallel is ChatGPT's Dots - always-on agents you simply talk to,
which start other agents for you. Home is what Dots would be for Codex.

**Tasks come second.** The Tasks destination is as close to Codex chats as
possible, but it holds only work - runs, grouped into Projects and Agents. Most
people will rarely need it, because Home hands the tasks out.

A new task initially shows a preview of its request. With the native Codex
provider, a background request to the selected model replaces that preview with
a concise title. Naming does not delay the work or overwrite an authored name
or a manual sidebar rename.

Opening Working or a tool's details keeps the selected header in place while
the contents expand and new output arrives. Details the reader opened remain
open when the answer finishes. Scrolling to the bottom or choosing the latest
message resumes following new output.
Resume is offered after a confirmed stop or interruption. An ordinary failed
reply keeps the error visible and accepts a new request; it does not imply that
a resumable native turn exists.

Task progress counts visible, unarchived tasks in Home's environment tree.
Internal service runs do not appear in its totals. Failed tasks remain distinct
from completed work, and an empty workspace shows "No tasks yet".

**Dashboard** is the least necessary destination for an ordinary user and a
convenient one for everyone else: Topology, Registry, Worker Environments, and
Movements.

**Inbox** is literally an inbox. Messages from applications, requests that
need an answer, and notifications from the system - including marks and
verifier results - all arrive there. Home and Inbox are most of what a user
needs.

**Applications** are the nearest thing to Codex plugins: providers,
connectors, plugins, and knowledge packages.

**Drivers** are heavier: the rooms where tools act - a folder, a worktree, a
browser, a desktop, a container. Codex has no parallel; this is a capability of
the architecture.

**Extensions** belong to the client alone: widgets, workspace tabs, and guided
repairs. Codex has no equivalent. The closest relative is DeepSeek Harness and
its everything-is-a-plugin design; in Octopus it is a whole category of the
client.

Everything else follows Codex: a sidebar, a working pane opened beside the
conversation, split panes.

## Home chooses when to act

Codex decides by itself when memory is worth reading: a short decision
boundary, a compact summary already in context, and tools for detail. Home
works the same way for everything it can do.

- **The boundary is short.** Home's role says when a turn needs only an
  answer, when to start a task, set a trigger, show a widget, save knowledge,
  change the client's own code, or change Octopus itself - and when to do none of
  these. Short actions use Home's own granted tools immediately, including
  browser tools. Longer work starts a separate task so the conversation can
  continue. When the operator states a goal, Home starts the work instead of
  asking for permission, and when unsure it does the cheap reversible part and
  offers the rest in one line.
- **The summary is what you see.** Every message sent from Home carries the
  client's snapshot of the tasks with their status and last reply, the
  triggers with their schedule, the projects and the Agents. It travels inside
  the message artifact, so the ledger records exactly what Home knew. Text
  input and voice carry an explicit client observation of available surfaces
  and the widget inventory, including exact IDs, source digests, visibility,
  configuration and review availability. Home can use these identities directly;
  listing is for missing or stale detail. The observation fits within 4 KiB,
  keeps whole rows and counts omissions; Home state has 12 KiB, with a shared
  16 KiB limit. A text retry retains its original saved observation. During a
  call, only changed observations cross the control interface, which retains
  them for delegated requests without sending them to the audio model. Each voice
  delegation carries the last durably accepted snapshot. Older calls may
  have none. Observations grant no authority: each action still checks its
  admitted route and current source digest.
- **Voice follows the work.** Public progress and answers reach the active call
  as they appear. Completed text is not spoken a second time. Updates from an
  earlier call cannot become answers to a new request.
- **Acting is cheap.** A task is one call: a title and the assignment. The
  installation's defaults pick the environment, worker, model and tools and
  bring the result back to Home. Procedures for hand-authored runs, widget
  design and the rest stay in artifacts the model reads only when it needs
  them; prose that is always in context is the expensive kind.

Home is expected to notice things in that snapshot - a failed task, results
nobody has looked at, a request that keeps coming back - and propose concrete
work, never generic suggestions.

## Transparency

A lot looks hidden and is not. In Home, **Activity** shows what is happening:
the conversation and the full trace of its work. Nothing about that has to be
in the way. You can just talk; when you want to see, you look.

## It feels like an operating system

**The system is started, not just opened.** The client is separate from the
system. When you open it, the system is not running yet: the embedded terminal
prepares `octopus up`, and you start it yourself. The client never starts the
system on its own. A local password guards the client. This is deliberate - it
is the moment you feel that this is an operating system.

**Onboarding sets up an environment, not a project.** A short welcome, then you
choose a setup - Life, Work, Founder, or Custom - name your areas and projects,
and pick capabilities. Creating projects at onboarding would make no sense in
Codex; here you are defining the operating environment the system will live
in - its goals. A preset materializes into ordinary execution environments and
bindings; there is no preset template in the runtime. Apply is a resumable
client transaction, repeating it creates no duplicates, and your first real
message stays the first message in Home.

**A setup travels as a file.** The choices onboarding collects - the space,
its project tree with names, goals, icons and colors, the capabilities and the
projects each one serves, and optionally the Home model - export as one
`*.octopus-setup.json` file (`"schema": "octopus.setup"`, version 1) and import
on another machine at the first step. Setup's review step exports its choices;
**Settings → General → Setup file** exports a Home that is already running, as
the client shows it: its projects, the capabilities installed, and the projects
each one is available in. Importing fills the steps for review; it
does not set anything up by itself. A file that names a component or a model
this Octopus does not offer is refused with that name, never trimmed silently.
The file carries no keys, accounts, runs or history.

**States stay honest.** Available, installed, configured, available in a
project, connected to a task, and live are separate facts. No earlier one is
presented as proof of the next.

## Where the interface is heading

Codex is a workbench. Claude Code is a coder's environment. Octopus is an
operating system you should not need to leave: make a widget to watch
notifications, set up an automation, give the system an interface through
which it tells you when something needs you.

The camera and gestures are part of that direction - an interface for
communicating, not only for pointing. A gesture can open a second window, move
it, or send a recorded voice prompt. Gestures that only arrange the client stay
client input; a gesture that sends something to a model enters the ordinary
chat or voice path.

## Triggers instead of schedules

Elsewhere, a schedule runs something at a time, and Codex hooks react to its
own session lifecycle - session start, a submitted prompt, before and after a
tool call, stop. In Octopus a trigger is an interface with its own rules. An
interface can check anything and send anything whenever its conditions hold,
so a trigger is simply a run with such an interface attached in JSON: at this
time, or after this event, check this and change that.

Creating a trigger opens a normal task draft. The operator describes what
should happen and when; the model chooses the interface, target run and
bindings using the bound `artifact-trigger-authoring` guide and architecture.
Suggestions fill the draft without sending it. The model confirms the useful
behavior in plain language after checking the installed configuration and
live binding. Existing triggers retain their direct editing controls.

## Self-improvement

Self-evolution is visible in how the system reshapes the conditions of its
future work: its goals, triggers, tools, workers, context, and environments.
Those changes persist and influence its trajectory across tasks and runs.
Self-improvement is the aim when those changes make the system better.

Self-improvement has to feel as native as everything else, and it is
configured through JSON rather than added as a feature. A trigger interface
that, after a run goes quiet, sends a prompt to a run that looks for ways the
system could improve - a widget, a skill, a new interface - is ordinary
configuration over the existing primitives. The platform makes this possible;
finding what is worth building on it takes creativity.

The System review package ships that configuration as a trigger template, and
setup arms it: a new Home starts with a live trigger that waits for an hour
without work. In Triggers you change the quiet period and whether all work or
one project counts; when that much time passes without work, one ordinary task
starts with the review prompt. It reads what actually happened and may change memory,
knowledge and skills, Home widgets, runs, triggers, interfaces or package
code - or nothing, which is often the right answer. The task keeps a change
journal with the exact earlier bytes of every file it touched and the checks
it ran. Triggers lists each review with what it changed, and undoes one in a
step while the files still hold what the review wrote.

See [Architecture](architecture.md#autonomy-and-attention) for the research
question and the relationship between self-evolution, attention, and trajectory.
