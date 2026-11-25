#!/usr/bin/env node
const fs = require("node:fs/promises");
const path = require("node:path");
const { parseArgs } = require("node:util");

const HELP = `Octopus — your agentic operating system, in the terminal

  octopus [prompt]                 Open the interactive client
  octopus resume [task|last|home]  Continue an existing task
  octopus exec <prompt>            Start a task and wait for its result
  octopus send <task> <prompt>     Send a follow-up and wait
  octopus watch <task>             Follow work already in progress
  octopus tasks                   Show tasks from this Home
  octopus history <task>           Read its transcript
  octopus inspect <run-id>         Inspect recorded checkpoint and transfer evidence
  octopus projects                Show Projects and Agents
  octopus project create <name>   Create a Project with the parent's policy
  octopus project folders <id> <folder>...  Set a Project's source folders (first is Primary)
  octopus project policies        Show policies available to a parent Project
  octopus project delete <id> --yes  Delete a Project
  octopus agent create <name>     Create an Agent with an explicit model
  octopus agent delete <id> --yes  Delete an Agent
  octopus models                  Show available permitted models
  octopus answer <task> <request>  Answer a pending question
  octopus stop <task>              Ask the task to stop
  octopus rename <task> <title>    Rename a task
  octopus archive <task>           Archive a settled task
  octopus restore <task>           Restore an archived task
  octopus delete <task> --yes       Delete a settled task; retain recorded evidence
  octopus export <task> <file>     Save a transcript (new file only)
  octopus download <task> <attachment-id> <file>  Save an admitted attachment
  octopus setup                    Set up a new Home interactively
  octopus connect [status|disconnect] <app>  Connect, check, or disconnect an Application
  octopus up [--foreground]        Start or join the shared runtime (or run in foreground)
  octopus down                     Stop the running shared runtime cleanly
  octopus restart                  Restart the running shared runtime in place
  octopus status                   Inspect runtime and Home readiness
  octopus doctor                   Verify the active registry, client packages, and probes

Options:
  --home <path>                   The same Home used by the desktop client
  --project <id> --agent <id>      Choose the Project and named Agent
  --provider <id> --model <id>     Explicit provider/model selection
  --effort <level>                Reasoning effort offered by that model
  --service-tier <id>             Service tier offered by the model
  --policy <id>                  Installed policy for a new Project
  --attach <path>                 Attach a file or folder; repeatable
  --access <mode>                 New task access: project_access or full_access
  --json                          JSON results; NDJSON for streamed work
  --foreground                    Keep 'up' attached in the foreground (for systemd)
  --no-start                      Provision packages and Home without starting the runtime daemon
  --no-wait                       Return the input admission receipt
  --timeout <seconds>             Stop waiting at a deadline, leave task running
                                  up: omitted or 0 uses the 120-second default
  --output-last-message <file>    Write the final reply to a new file
  --archived                      List archived tasks
  --yes                           Confirm task deletion
  --answer <json>                 Structured answer to a pending request
  --decision <choice>             Approval choice (accept/decline/cancel)
  --bundle <path>                 Explicit compatible runtime distribution
  -h, --help                      Show this help

Use 'exec -' or 'send <task> -' to read a prompt from stdin.
In chat: /help, /new, /tasks, /project, /agent, /model, /attach, /stop.
CLI exit leaves the shared runtime and admitted work running.
`;

function argumentsFor(argv) {
  const result = parseArgs({
    args: argv, allowPositionals: true, strict: true,
    options: {
      help: { type: "boolean", short: "h" }, version: { type: "boolean", short: "v" },
      home: { type: "string" }, bundle: { type: "string" },
      project: { type: "string" }, agent: { type: "string" },
      provider: { type: "string" }, model: { type: "string" }, effort: { type: "string" }, policy: { type: "string" },
      attach: { type: "string", multiple: true, default: [] },
      application: { type: "string", multiple: true, default: [] },
      access: { type: "string" },
      foreground: { type: "boolean", default: false },
      "no-start": { type: "boolean", default: false },
      json: { type: "boolean", default: false }, "no-wait": { type: "boolean", default: false },
      timeout: { type: "string" }, "output-last-message": { type: "string" },
      archived: { type: "boolean", default: false },
      yes: { type: "boolean", default: false },
      answer: { type: "string" }, decision: { type: "string" },
      "workspace-name": { type: "string" },
      "project-name": { type: "string" }, "pack": { type: "string" },
      "non-interactive": { type: "boolean", default: false },
      "sign-in": { type: "boolean", default: false },
      "service-tier": { type: "string" },
    },
  });
  const seconds = result.values.timeout === undefined ? 0 : Number(result.values.timeout);
  if (!Number.isFinite(seconds) || seconds < 0 || seconds > 86_400) throw new Error("--timeout must be between 0 and 86400 seconds.");
  return { ...result, timeoutMs: seconds * 1000 };
}

async function main(argv = process.argv.slice(2), io = process) {
  let client;
  let environment;
  let json = false;
  let signalHandler;
  try {
    const parsed = argumentsFor(argv);
    const flags = parsed.values;
    json = flags.json;
    if (flags.help) { io.stdout.write(commandHelp(parsed.positionals)); return 0; }
    if (flags.version) { io.stdout.write(`Octopus ${require("../package.json").version}\n`); return 0; }
    if (Number(process.versions.node.split(".")[0]) < 24) throw new Error("Octopus CLI needs Node.js 24 or newer. Set OCTOPUS_NODE to that Node executable.");
    try { await fs.access(path.join(__dirname, "../build/cli/shared.cjs")); }
    catch (error) { if (error.code === "ENOENT") throw new Error("Build the terminal client first: npm --prefix app run build:cli"); throw error; }
    const { createEnvironment } = require("./environment.cjs");
    environment = createEnvironment({ home: flags.home, bundle: flags.bundle });
    const selection = {
      projectId: flags.project, agentId: flags.agent,
      providerId: flags.provider, model: flags.model, effort: flags.effort, serviceTier: flags["service-tier"],
      files: flags.attach,
      ...(flags.access === undefined ? {} : { access: taskAccess(flags.access) }),
    };
    const words = [...parsed.positionals];
    const known = new Set(["chat", "resume", "exec", "send", "watch", "tasks", "history", "inspect", "projects", "models", "answer", "stop", "rename", "archive", "restore", "delete", "export", "setup", "connect", "up", "down", "restart", "status", "doctor", "agent", "project", "download"]);
    const command = known.has(words[0]) ? words.shift() : "chat";
    if (flags.foreground && command !== "up") throw new Error("--foreground is only supported with 'up'.");
    if (flags["no-start"] && command !== "setup") throw new Error("--no-start is only supported with 'setup'.");
    const report = (value) => {
      if (json) io.stdout.write(`${JSON.stringify(value)}\n`);
      else io.stdout.write(`${safeText(presentResult(value))}\n`);
    };
    if (command === "status") { report(await environment.status()); return 0; }
    if (command === "up") {
      if (words.length) throw new Error("up does not accept positional arguments. See --help.");
      if (flags.foreground) {
        if (json) throw new Error("--foreground cannot be combined with --json.");
        if (flags.timeout !== undefined) throw new Error("--foreground cannot be combined with --timeout.");
        const result = await environment.startForeground();
        return result.exitCode;
      }
      report(await environment.start({ timeoutMs: parsed.timeoutMs > 0 ? parsed.timeoutMs : undefined,
        onProgress: (value) => progress(io, json, value.phase.replaceAll("_", " ")) }));
      return 0;
    }
    if (command === "down" || command === "restart" || command === "doctor") {
      if (words.length) throw new Error(`${command} does not accept positional arguments. See --help.`);
      const controller = new AbortController();
      let interrupted = false;
      signalHandler = () => { interrupted = true; controller.abort(); };
      process.on("SIGINT", signalHandler);
      try {
        const result = await environment[command]({
          signal: controller.signal,
          ...(json ? {} : {
            onStdout: (text) => io.stdout.write(safeText(text)),
            onStderr: (text) => io.stderr.write(safeText(text)),
          }),
        });
        if (json) report(result);
        return result.exitCode;
      } catch (error) {
        if (interrupted && error?.code === "ABORT_ERR") {
          if (json) report({ type: "error", code: "interrupted", message: error.message });
          else progress(io, false, `Interrupted '${command}'.`);
          return 130;
        }
        throw error;
      }
    }
    if (command === "connect") {
      const { connectApplication, presentConnectResult } = require("./connect.cjs");
      const controller = new AbortController();
      let interrupted = false;
      signalHandler = () => { interrupted = true; controller.abort(); };
      process.on("SIGINT", signalHandler);
      try {
        const result = await connectApplication(environment, {
          words,
          signal: controller.signal,
          onProgress: (update) => progress(io, json, update.message),
          openExternal: (url) => { io.stderr.write(safeText(`\nOpen this address to sign in:\n${url}\n`)); },
        });
        if (json) report(result);
        else io.stdout.write(`${safeText(presentConnectResult(result))}\n`);
        return 0;
      } catch (error) {
        if (interrupted && error?.code === "cancelled") {
          if (json) report({ type: "error", code: "interrupted", message: error.message });
          else progress(io, false, "Interrupted 'connect'.");
          return 130;
        }
        throw error;
      }
    }
    let terminal;
    if (command === "chat" || command === "resume" || command === "setup") {
      if (!io.stdin.isTTY || !io.stdout.isTTY) {
        if (!(command === "setup" && flags["non-interactive"])) throw new Error("Interactive Octopus needs a terminal. Use 'exec', 'send', or '--non-interactive' setup for automation.");
      }
      if (!(command === "setup" && flags["non-interactive"])) terminal = require("./terminal.cjs");
    }
    if (command === "setup") {
      const ui = flags["non-interactive"]
        ? {
            openExternal(url) {
              (json ? io.stderr : io.stdout).write(safeText(`\nOpen this address to sign in:\n${url}\n`));
            },
            close() {},
          }
        : (terminal?.createSetupUI ? terminal.createSetupUI({ input: io.stdin, output: io.stdout }) : setupUI(io));
      let lastProgressLine = "";
      const onProgress = (value) => {
        const line = formatSetupProgress(value);
        if (!line || line === lastProgressLine) return;
        lastProgressLine = line;
        progress(io, json, line);
      };
      try {
        const result = await environment.setup({
          ui, nonInteractive: flags["non-interactive"], packId: flags.pack,
          workspaceName: flags["workspace-name"], projectName: flags["project-name"],
          providerId: flags.provider,
          model: flags.model, reasoningEffort: flags.effort, serviceTier: flags["service-tier"],
          signIn: flags["non-interactive"] ? flags["sign-in"] : true, recommended: [],
          noStart: Boolean(flags["no-start"]),
          onProgress,
        });
        report(result);
        if (!json && result?.runtime?.skipped === true) {
          progress(io, false, "Runtime daemon not started (--no-start). Start with 'octopus up' or your service manager.");
        }
      } finally { ui.close?.(); }
      return 0;
    }
    if (command === "chat" || command === "resume") {
      const runtime = await environment.start({ onProgress: (value) => progress(io, false, value.phase.replaceAll("_", " ")) });
      if (!runtime.clientReady) {
        const ui = setupUI(io);
        try { await environment.setup({ ui, recommended: [] }); }
        finally { ui.close(); }
      }
    }
    environment.requireDistribution?.();
    const { createClient } = require("./client.cjs");
    client = createClient(environment);
    if (command === "chat" || command === "resume") {
      const run = command === "resume" ? await client.task(words.shift() || "last") : null;
      return await terminal.runInteractive(client, {
        ...selection, input: io.stdin, output: io.stdout,
        runId: run?.id, initialPrompt: words.join(" "),
      }) ?? 0;
    }
    if (command === "tasks") { report(await client.tasks({ archived: flags.archived, search: words.join(" ") })); return 0; }
    if (command === "projects") { report((await client.contexts()).projects); return 0; }
    if (command === "models") { report(await client.models(selection)); return 0; }
    if (command === "history") { report(await client.history(required(words.shift(), "task"))); return 0; }
    if (command === "inspect") {
      const selected = required(words.shift(), "exact run id");
      const runId = ["home", "last"].includes(selected) ? (await client.task(selected)).id : selected;
      const inspection = await require("./inspection.cjs").inspectRun(client, runId);
      if (json) report(inspection);
      else io.stdout.write(`${safeText(presentInspection(inspection))}\n`);
      return 0;
    }
    if (command === "stop") { report(await client.stop(required(words.shift(), "task"))); return 0; }
    if (command === "rename") { report(await client.rename(required(words.shift(), "task"), required(words.join(" "), "title"))); return 0; }
    if (command === "archive" || command === "restore") { report(await client.archive(required(words.shift(), "task"), command === "archive")); return 0; }
    if (command === "delete") {
      if (!flags.yes) throw new Error("Task deletion removes its active record and private interfaces. Use --yes to confirm; recorded evidence is retained.");
      report(await client.deleteTask(required(words.shift(), "task"))); return 0;
    }
    if (command === "agent") {
      const action = words.shift();
      if (action === "delete") {
        if (!flags.yes) throw new Error("Agent deletion requires --yes.");
        report(await client.deleteAgent(required(words.shift(), "Agent"))); return 0;
      }
      if (action !== "create") throw new Error("Use 'agent create <name> --project <id> --provider <id> --model <id> --effort <level> [--application <id>]...' or 'agent delete <id> --yes'.");
      report(await client.createAgent({ ...selection, name: required(words.join(" "), "Agent name"), ...(flags.application.length ? { applicationIds: flags.application } : {}) })); return 0;
    }
    if (command === "project") {
      const action = words.shift();
      if (action === "policies") { report(await client.projectPolicies(flags.project)); return 0; }
      if (action === "delete") {
        if (!flags.yes) throw new Error("Project deletion requires --yes.");
        report(await client.deleteProject(required(words.shift(), "Project"))); return 0;
      }
      if (action === "folders") {
        const projectId = required(words.shift(), "Project");
        if (!words.length) throw new Error("Use 'project folders <project-id> <folder>...'; the first folder is Primary.");
        report(await client.setProjectFolders(projectId, words.map((folder) => path.resolve(folder)))); return 0;
      }
      if (action !== "create") throw new Error("Use 'project create <name>', 'project folders <project-id> <folder>...' or 'project policies'.");
      report(await client.createProject({ name: required(words.join(" "), "Project name"), parentId: flags.project, policy: flags.policy })); return 0;
    }
    if (command === "answer") {
      const task = required(words.shift(), "task");
      const request = required(words.shift(), "request id");
      const answer = flags.answer === undefined ? { decision: required(flags.decision, "--answer or --decision") } : JSON.parse(flags.answer);
      report(await client.respond(task, request, answer)); return 0;
    }
    if (command === "download") {
      report(await client.saveAttachment(required(words.shift(), "task"), required(words.shift(), "attachment id"), required(words.shift(), "destination")));
      return 0;
    }
    if (command === "export") {
      const entries = await client.history(required(words.shift(), "task"));
      const destination = path.resolve(required(words.shift(), "destination"));
      await fs.writeFile(destination, presentResult(entries), { flag: "wx", mode: 0o600 });
      report({ path: destination, messages: entries.length }); return 0;
    }
    let submitted;
    let runId;
    if (command === "watch") {
      const task = await client.task(required(words.shift(), "task"));
      runId = task.id;
    } else {
      if (command === "send") runId = (await client.task(required(words.shift(), "task"))).id;
      const prompt = words.join(" ");
      const text = prompt === "-" ? await stdinText(io.stdin) : prompt;
      if (!text && !selection.files.length) throw new Error("Provide a prompt or --attach a file.");
      submitted = await client.send({ ...selection, runId, text });
      runId = submitted.runId;
      if (json) report({ type: "admitted", runId, messageId: submitted.messageId, afterSeq: submitted.afterSeq });
      else progress(io, false, `Task ${runId}`);
      if (flags["no-wait"]) { if (!json) report({ runId, status: "admitted" }); return 0; }
    }
    const controller = new AbortController();
    let interrupted = false;
    signalHandler = () => { interrupted = true; controller.abort(); };
    process.on("SIGINT", signalHandler);
    const deadline = parsed.timeoutMs ? setTimeout(() => controller.abort(), parsed.timeoutMs) : null;
    const messages = new Map();
    const selectedTurn = { sourceMovementSeq: submitted?.afterSeq };
    const renderer = createEventRenderer(io);
    let lastMessage = "";
    let failed = false;
    let completed = false;
    let stopped = false;
    let pending = false;
    try {
      for await (const event of client.follow(runId, { ...submitted, signal: controller.signal })) {
        if (json) report(event);
        else renderer(event);
        if (event.type === "message" && event.entry?.role === "assistant" && ["message", "message_delta"].includes(event.entry.kind)) {
          messages.delete(event.entry.id);
          messages.set(event.entry.id, event.entry);
        }
        if (event.type === "delta" && event.role === "assistant" && messages.has(event.id)) {
          const entry = { ...messages.get(event.id), text: messages.get(event.id).text + event.text };
          messages.delete(event.id);
          messages.set(event.id, entry);
        }
        if (event.type === "status" && event.correlated) {
          const metadata = event.entry?.metadata;
          for (const field of ["runId", "interfaceId", "fromInterfaceId", "fromInterfaceRole", "direction", "turnId", "threadId"]) {
            if (metadata?.[field]) selectedTurn[field] = metadata[field];
          }
          if (event.turnId) selectedTurn.turnId = event.turnId;
          if (Number.isSafeInteger(metadata?.sourceMovementSeq)) selectedTurn.sourceMovementSeq = metadata.sourceMovementSeq;
        }
        if (event.type === "status" && event.terminal && event.correlated) {
          completed = true;
          failed = event.state === "failed";
          stopped = event.interrupted === true;
          lastMessage = [...messages.values()].findLast((entry) => {
            if (!entry.text) return false;
            if (entry.id === event.entry?.id) return true;
            const metadata = entry.metadata;
            if (!metadata || metadata.fromInterfaceRole !== "provider" || metadata.direction !== "out"
              || ["runId", "interfaceId", "fromInterfaceId"].some((field) => metadata[field] !== selectedTurn[field])
              || ["turnId", "threadId"].some((field) => metadata[field] && selectedTurn[field] && metadata[field] !== selectedTurn[field])) return false;
            return Boolean(selectedTurn.turnId && metadata.turnId === selectedTurn.turnId)
              || Number.isSafeInteger(selectedTurn.sourceMovementSeq) && metadata.sourceMovementSeq === selectedTurn.sourceMovementSeq;
          })?.text || "";
        }
        if (event.type === "request" && event.status !== "resolved") { pending = true; break; }
      }
    } finally { if (deadline) clearTimeout(deadline); }
    if (interrupted) { progress(io, json, `Detached. Task ${runId} continues; use 'octopus stop ${runId}' to stop it.`); return 130; }
    if (controller.signal.aborted) { report({ type: "timeout", runId, status: "still_running" }); return 124; }
    if (pending) { if (!json) progress(io, false, `An answer is required. Resume ${runId} or use 'octopus answer'.`); return 3; }
    if (!completed) throw new Error("Following ended without a recorded completion. Use 'watch' or 'history' to inspect the task.");
    if (flags["output-last-message"]) await fs.writeFile(path.resolve(flags["output-last-message"]), lastMessage, { flag: "wx", mode: 0o600 });
    if (json) report({ type: "result", runId, status: failed ? "failed" : stopped ? "stopped" : "completed", text: lastMessage });
    else io.stdout.write("\n");
    return failed ? 1 : stopped ? 130 : 0;
  } catch (error) {
    if (json) io.stdout.write(`${JSON.stringify({ type: "error", code: error.code || "cli_error", message: error.message })}\n`);
    else io.stderr.write(`Octopus: ${safeText(String(error.message))}\n`);
    return 1;
  } finally {
    if (signalHandler) process.removeListener("SIGINT", signalHandler);
    await client?.close();
    environment?.close();
  }
}

function setupUI(io) {
  const readline = require("node:readline/promises");
  const rl = readline.createInterface({ input: io.stdin, output: io.stdout });
  return {
    async ask({ prompt, defaultValue }) {
      const answer = await rl.question(safeText(`${prompt}${defaultValue ? ` [${defaultValue}]` : ""}: `));
      return answer || defaultValue || "";
    },
    async choose({ prompt, choices, defaultValue }) {
      io.stdout.write(safeText(`\n${prompt}\n${choices.map((row, index) => `  ${index + 1}. ${row.label}${row.description ? ` — ${row.description}` : ""}`).join("\n")}\n`));
      for (;;) {
        const answer = await rl.question(`Choose${defaultValue ? " (Enter for default)" : ""}: `);
        if (!answer && defaultValue !== undefined) return defaultValue;
        const selected = choices[Number(answer) - 1];
        if (selected) return selected.value;
        io.stdout.write("Enter one of the listed numbers.\n");
      }
    },
    openExternal(url) { io.stdout.write(safeText(`\nOpen this address to sign in:\n${url}\n`)); },
    progress(value) { progress(io, false, value.label || value.phase || String(value)); },
    close() { rl.close(); },
  };
}

function createEventRenderer(io) {
  const printed = new Map();
  const activities = new Map();
  let lastId = "";
  return (event) => {
    if (event.type === "status" && event.correlated && event.state === "stopping") {
      progress(io, false, "Stop acknowledged. Waiting for the current operation to report its result.");
      return;
    }
    if (event.type === "status" && event.terminal && event.correlated && event.interrupted) {
      progress(io, false, event.entry?.kind === "interrupted_result" ? "Stopped." : "Stop acknowledged.");
      return;
    }
    if (event.entry && !require("./presentation.cjs").isConversationEntry(event.entry)) return;
    if (event.type === "delta") {
      if (lastId !== event.id) io.stdout.write("\n");
      io.stdout.write(safeText(event.text || ""));
      printed.set(event.id, (printed.get(event.id) || "") + (event.text || ""));
      lastId = event.id;
    } else if (event.type === "message" && event.entry?.text) {
      const { id, text } = event.entry;
      const previous = printed.get(id) || "";
      if (text !== previous) {
        const suffix = text.startsWith(previous) && lastId === id;
        io.stdout.write(suffix ? safeText(text.slice(previous.length)) : `\n${safeText(text)}`);
        printed.set(id, text);
        lastId = id;
      }
    } else if (event.type === "activity") {
      const detail = [
        `${event.title || "Working"}${event.status ? ` · ${event.status}` : ""}${Number.isInteger(event.exitCode) ? ` · exit ${event.exitCode}` : ""}`,
        event.script || event.command,
        event.output || [event.stdout, event.stderr].filter(Boolean).join("\n"),
        event.sections?.some((section) => section.stream === "error") ? null : event.failure,
        ...(event.notices || []),
      ].filter(Boolean).join("\n");
      if (activities.get(event.id) !== detail) progress(io, false, detail);
      activities.set(event.id, detail);
    } else if (event.type === "request" && event.status !== "resolved") {
      io.stderr.write(`\n${safeText(event.entry?.text || "Input required")}\n${safeText(JSON.stringify({ requestId: event.requestId || event.payload?.request_id, kind: event.kind, request: event.payload }))}\n`);
    } else if (event.type === "error") progress(io, false, event.text || "Task failed");
  };
}

function safeText(text) { return require("./presentation.cjs").safeText(text); }

function commandHelp(words) {
  if (words[0] === "project") return `Octopus Projects\n\n  octopus project create "Project name" [--project <parent-id>] [--policy <id>]\n  octopus project policies [--project <parent-id>] [--json]\n  octopus project folders <project-id> <folder>... [--json]\n\nNew Projects use their parent's policy unless an allowed policy is selected.\nThe first folder is the Project's Primary folder; editing folders affects future tasks.\nUse 'octopus projects' to find existing Project IDs.\n`;
  if (words[0] === "agent") return `Octopus Agents\n\n  octopus agent create "Agent name" --project <id> --provider <id> --model <id> --effort <level> [--application <id>]...\n\nUse 'octopus projects --json' for Project IDs and 'octopus models --project <id> --json'\nfor the exact permitted provider, model, and reasoning choices.\nThe Agent uses that Project's configured Drivers and its default Applications;\n--application selects the Applications explicitly instead.\n`;
  if (words[0] === "connect") return `Octopus connect\n\n  octopus connect <application-id> [interface-id] [--home <path>] [--json]\n  octopus connect status <application-id> [interface-id] [--home <path>] [--json]\n  octopus connect disconnect <application-id> [interface-id] [--home <path>] [--json]\n\nConnects, checks, or disconnects an installed client_connection Application (for example world_provider_antigravity).\nProgress and sign-in addresses are written to stderr.\n`;
  if (words[0] === "setup") return `Octopus setup\n\n  octopus setup [--home <path>]\n  octopus setup --home <path> --non-interactive --pack <id> --workspace-name <name>\n    --project-name <name> [--provider <id>] --model <id> --effort <level> [--no-start]\n\nInteractive setup lists installed choices and guides provider sign-in or credentials.\nNoninteractive setup requires existing provider sign-in or credentials (for API-key providers, via the provider's auth_env_key or saved private environment.json). Configured Homes are preserved.\nUse --no-start to provision packages, provider credentials, and Home without starting the runtime daemon (for systemd/service managers).\n`;
  return HELP;
}

function formatSetupProgress(value) {
  if (!value || typeof value !== "object") return value ? `[setup] ${String(value)}` : "";
  if (value.packageId) {
    const phase = value.phase ? String(value.phase).replaceAll("_", " ") : "working";
    const percent = Number.isFinite(value.percent) ? ` (${value.percent}%)` : "";
    return `[setup] packages: ${phase} ${value.packageId}${percent}`;
  }
  if (value.step) {
    const phase = value.phase ? String(value.phase).replaceAll("_", " ") : "working";
    return `[setup] ${value.step}: ${phase}`;
  }
  if (value.label) return `[setup] ${value.label}`;
  if (value.phase) return `[setup] ${String(value.phase).replaceAll("_", " ")}`;
  return "";
}

function presentResult(value) {
  if (Array.isArray(value) && value.every((row) => row.role && row.kind)) {
    const { entryRows } = require("./presentation.cjs");
    return value.flatMap((row) => entryRows(row, 100, { outputRows: Number.MAX_SAFE_INTEGER }).map((line) => line.text)).join("\n");
  }
  if (Array.isArray(value)) return value.map((row) => row.text !== undefined
    ? `${row.role || row.kind || ""}\n${row.text}`
    : `${row.id || row.model || ""}  ${row.title || row.name || row.displayName || ""}${row.state ? `  ${row.state}` : ""}${row.agents ? `\n${row.agents.map((agent) => `  ${agent.id}  ${agent.name}  ${agent.model || "choose model"}`).join("\n")}` : ""}`).join("\n");
  return JSON.stringify(value, null, 2);
}

function progress(io, json, text) {
  io.stderr.write(`${safeText(String(text))}\n`);
}

function presentInspection(value) {
  const active = value.checkpoints.filter((checkpoint) => checkpoint.active);
  const lines = [value.runId, `${active.length} active checkpoints · ${value.outstanding.length} requirements without an exact recorded mark`];
  for (const wait of value.outstanding) lines.push(`  ${wait.artifactLabel || "Unknown artifact"} → ${wait.mark || "Unknown mark"} (${wait.state})\n    ${wait.checkpointId}`);
  lines.push(`${value.relay.candidates.length} recorded outbound transfers match active interface bindings.`);
  if (!value.coverage.runProjectionStable) lines.push("The run changed during inspection; read again for a consistent view.");
  lines.push("This shows recorded evidence. Current admission and policy remain runtime decisions.");
  return lines.join("\n");
}

function required(value, label) {
  if (typeof value !== "string" || !value.trim()) throw new Error(`${label} is required. See --help.`);
  return value;
}

async function stdinText(input) {
  const parts = [];
  let length = 0;
  for await (const chunk of input) {
    length += Buffer.byteLength(chunk);
    if (length > 4 * 1024 * 1024) throw new Error("The stdin prompt exceeds 4 MiB; attach the file instead.");
    parts.push(Buffer.from(chunk));
  }
  return Buffer.concat(parts).toString("utf8");
}

if (require.main === module) main().then((code) => { process.exitCode = code; }).catch((error) => {
  process.stderr.write(`Octopus cleanup failed: ${error.message}\n`); process.exitCode = 1;
});

function taskAccess(value) {
  if (value === "project_access" || value === "full_access") return value;
  throw new Error("--access must be project_access or full_access.");
}

module.exports = { main, argumentsFor, HELP, createEventRenderer, formatSetupProgress };
