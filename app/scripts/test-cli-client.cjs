const assert = require("node:assert/strict");
const { test } = require("node:test");
const fs = require("node:fs/promises");
const os = require("node:os");
const path = require("node:path");
const { createClient } = require("../cli/client.cjs");
const { argumentsFor, createEventRenderer } = require("../cli/index.cjs");

const run = { run_id: "run_task", chat_interface_id: "chat_task", provider_interface_id: "provider",
  provider_bound_interface_id: "provider_task", client_surface_role: "task", writable: true, turn_open: true, turn_state: "working" };
const receipt = { runId: run.run_id, chatInterfaceId: run.chat_interface_id, modelInterfaceId: run.provider_bound_interface_id,
  messageId: "message", messageVerdict: { admitted: { seq: 100 } }, afterSeq: 100, cursor: { offset: 10, seq: 100 } };

test("CLI projection readers leave publication recovery to its owning mutation", async () => {
  const { createEnvironment, projectionStore } = require("../cli/environment.cjs");
  const { projectionReadRequest } = require("../electron/projectionReadContract.cjs");
  const root = await fs.realpath(await fs.mkdtemp("/tmp/oct-cli-readers-")), repo = path.resolve(__dirname, "../..");
  const child = `const publication=require(process.argv[1]).nativePublication();const original=publication.publishFiles;const interrupted={...publication,publishFiles:async writes=>{await original(writes);process.exit(73)}};require(process.argv[2]).commitProjectionClientAuthoring(process.argv[3],process.argv[4],JSON.parse(process.argv[5]),{publication:interrupted}).catch(error=>{console.error(error);process.exit(74)});`;
  try {
    for (const kind of ["environment", "client"]) {
      const environment = createEnvironment({ home: path.join(root, kind), userHome: root, repoRoot: repo, installationRoot: repo, env: {} });
      const registry = environment.packageRoots.activeRegistryRoot, target = path.join(registry, "tool/fixture/settings.json"), content = '{"selected":"source"}\n';
      for (const directory of ["registry/interface", "registry/tool", "state/runs", "state/ee_instances"]) await fs.mkdir(path.join(environment.home, directory), { recursive: true });
      const request = { label: "CLI reader publication", removals: [], publishBarrierPath: target,
        writes: [{ file: target, content, beforeDigest: null, label: "settings" }] };
      const lost = require("node:child_process").spawnSync(process.execPath, ["-e", child,
        path.resolve(__dirname, "support/native-publication.cjs"), path.resolve(__dirname, "../electron/clientWorkspaceAuthoring.cjs"),
        environment.home, registry, JSON.stringify(request)], { encoding: "utf8", timeout: 20000 });
      assert.equal(lost.status, 73, lost.stderr);
      assert.equal(await fs.readFile(target, "utf8"), content);
      await assert.rejects(fs.access(path.join(registry, "interface/world_operator_chat/client.cjs")), { code: "ENOENT" });
      const transactions = path.join(environment.home, "client-state/workspace/transactions");
      const pending = (await fs.readdir(transactions)).filter(name => name !== ".octopus-publication");
      assert.equal(pending.length, 1);
      const journal = JSON.parse(await fs.readFile(path.join(transactions, pending[0], "journal.json")));
      const calls = [], source = environment.packageRoots.publication;
      environment.packageRoots.publication = Object.fromEntries(Object.keys(source).map(method => [method, async (...args) => {
        calls.push(method); return source[method](...args);
      }]));
      const reader = kind === "environment" ? projectionStore(environment) : createClient(environment);
      try {
        assert.equal(calls.length, 0);
        const result = kind === "environment" ? await reader.read(projectionReadRequest("projection.operator.os.workspace_setup"))
          : await reader.read("projection.operator.os.workspace_setup");
        assert.equal(result.status, "not_configured", JSON.stringify(result));
        assert.deepEqual(calls, []);
        assert.deepEqual((await fs.readdir(transactions)).filter(name => name !== ".octopus-publication"), pending);
        await require("../electron/workspaceSetup.cjs").recoverWorkspaceTransactions({
          octopusHomeRoot: environment.home, registryRoot: registry, publication: environment.packageRoots.publication,
        });
        const cleanup = JSON.parse(await fs.readFile(path.join(environment.home, "client-state/workspace/directory-cleanup", `${journal.operation_id}.json`)));
        assert.equal(cleanup.publication.reclaimed, true);
        assert.equal(cleanup.journal.writes[0].publication.receipt.operation_id, journal.writes[0].publication.operation_id);
        assert.equal(cleanup.journal.writes[0].publication.receipt.before_hash, null);
        assert.deepEqual((await fs.readdir(transactions)).filter(name => name !== ".octopus-publication"), []);
        for (const method of ["publishFiles", "snapshotDirectory", "publishDirectoryMoves", "reclaimDirectory"]) assert.ok(calls.includes(method), `${kind} omitted ${method}`);
      } finally { await reader.close(); environment.close(); }
    }
  } finally { await fs.rm(root, { recursive: true, force: true }); }
});

test("Stop uses scoped exact Home and run identities without decoding any conversation", async () => {
  const scope = { status: "ready", home_run_id: "run_home", home_interface_id: "chat_home", client_runs: [
    { run_id: "run_home", interface_id: "chat_home", surface_role: "home", archived: false },
    { run_id: "run_task", interface_id: "chat_task", surface_role: "task", archived: false },
    { run_id: "run_old", interface_id: "chat_old", surface_role: "task", archived: true },
  ] };
  const aliases = [scope.client_runs[2], scope.client_runs[1], scope.client_runs[0]].map(row => ({
    run_id: row.run_id, chat_interface_id: row.interface_id, title: row.run_id === "run_task" ? "Work" : row.run_id, writable: true,
  }));
  let aliasReads = 0, submitted = 0;
  const store = { async read({ method, params }) {
    if (method === "projection.operator.os.scope") return scope;
    assert.equal(method, "projection.operator.chats.list", "Stop must not request conversation payloads");
    assert.equal(params.control_targets_only, true); aliasReads++;
    return { chats: aliases };
  }, invalidate() {}, close() {} };
  const client = createClient({ home: os.tmpdir(), packageRoots: {}, status: async () => ({ runtimeReady: true }) }, {
    store, loader: { load: async () => ({ async submitOperatorTurnControl(_options, input) {
      submitted++; assert.equal(input.action, "stop"); return { ...input, verdict: { admitted: { seq: 1 } } };
    } }) }, metadata: {},
  });
  try {
    assert.equal((await client.stop("home")).interfaceId, "chat_home");
    assert.equal((await client.stop("run_task")).interfaceId, "chat_task");
    assert.equal(aliasReads, 0);
    for (const selector of ["last", "Work", "run_ta"]) assert.equal((await client.stop(selector)).runId, "run_task");
    assert.equal(aliasReads, 3);
    await assert.rejects(client.stop("run_"), /ambiguous/);
    await assert.rejects(client.stop("missing"), /No task/);
    await assert.rejects(client.stop("run_old"), /cannot accept Stop/);
    const before = submitted;
    scope.home_interface_id = "other"; await assert.rejects(client.stop("home"), /No Home/);
    scope.home_interface_id = "chat_home";
    aliases[1].chat_interface_id = "foreign"; await assert.rejects(client.stop("Work"), /differs/);
    scope.client_runs.push(scope.client_runs[1]); await assert.rejects(client.stop("run_task"), /invalid control target/);
    scope.client_runs.pop(); scope.status = "degraded"; await assert.rejects(client.stop("home"), /valid client scope/);
    assert.equal(submitted, before);
  } finally { await client.close(); }
});
function movement(seq, overrides = {}) {
  return { source: "movement", run_id: run.run_id, interface_id: run.chat_interface_id,
    from_interface_id: run.provider_bound_interface_id, from_interface_role: "provider",
    movement_seq: seq, recorded_at_unix_ms: seq, checkpoint_id: "cp", content_hash: "a".repeat(64),
    artifact_id: `reply:${seq}`, label: "interface.artifact.output", event_kind: "message",
    event_data: {}, text: "Reply", error: false, terminal: true, will_retry: false, ...overrides };
}
async function fixture(batches, check, { turnOpen = true, messages = [], authorize } = {}) {
  const home = await fs.mkdtemp(path.join(os.tmpdir(), "oct-cli-contract-"));
  let reads = 0;
  const store = {
    async read({ method }) {
      if (method === "projection.operator.chats.list") return { chats: [{ ...run, turn_open: turnOpen,
        turn_state: turnOpen ? "working" : "idle" }, { run_id: "internal_writer" }] };
      if (method === "projection.operator.chat.messages") return { messages };
      if (method === "projection.operator.chats.status") return { chats: [{ run_id: run.run_id,
        turn_open: turnOpen, turn_state: turnOpen ? "working" : "idle", turn_opened_movement_seq: 100, last_movement_seq: 110 }] };
      throw new Error(`Unexpected projection: ${method}`);
    }, close() {}, invalidate() {},
  };
  const transport = { assertGenericOperatorChatArtifactAllowed: authorize, async readOperatorChatArtifacts(_options, request) {
    assert.equal(request.runId, run.run_id);
    assert.equal(request.interfaceId, run.chat_interface_id);
    assert.equal(request.afterSeq, 100);
    const artifacts = typeof batches === "function" ? await batches(reads++, request) : batches[reads++];
    assert.ok(artifacts, "follow must stop at the genuine current terminal");
    return { artifacts, cursor: { offset: 100 + reads, seq: 100 + reads } };
  } };
  const client = createClient({ home, packageRoots: {}, status: async () => ({ runtimeReady: true }), accountClient() {} },
    { store, loader: { load: async () => transport }, metadata: {} });
  try { await check(client, () => reads); }
  finally { await client.close(); await fs.rm(home, { recursive: true, force: true }); }
}

test("the terminal lists only actual chat surfaces and follows durable exact-provider completion", async () => {
  const stream = { source: "stream", run_id: run.run_id, interface_id: run.chat_interface_id,
    from_interface_id: run.provider_bound_interface_id, from_interface_role: "provider", stream_id: "a".repeat(64),
    stream_seq: 1, byte_start_offset: 0, byte_end_offset: 80, source_movement_seq: 105, recorded_at_unix_ms: 105,
    event_kind: "turn_completed", event_data: { turn: { id: "turn", status: "completed" } }, terminal: true };
  await fixture([[stream], [movement(90)], [movement(111, { from_interface_id: "other_provider" })],
    [movement(112, { error: true, will_retry: true })], [movement(120)]], async (client, reads) => {
    assert.equal((await client.tasks()).length, 1);
    const events = await Array.fromAsync(client.follow(run.run_id, receipt));
    assert.equal(reads(), 5);
    const final = events.at(-1);
    assert.equal(final.type, "status");
    assert.equal(final.state, "idle");
    assert.equal(final.correlated, true);
    assert.equal(final.terminal, true);
  });
});

test("wrong task receipts fail before reading, detach does not send Stop, watch uses the recorded frontier", async () => {
  await fixture([ [movement(120)] ], async (client, reads) => {
    await assert.rejects(Array.fromAsync(client.follow(run.run_id, { ...receipt, modelInterfaceId: "other" })), /different task/);
    assert.equal(reads(), 0);
    const controller = new AbortController(); controller.abort();
    await Array.fromAsync(client.follow(run.run_id, { ...receipt, signal: controller.signal }));
    assert.equal(reads(), 0);
    const events = await Array.fromAsync(client.follow(run.run_id));
    assert.equal(events.at(-1).terminal, true);
    assert.equal(reads(), 1);
  });
});

test("an early idle projection cannot hide a final reply that has not arrived yet", async () => {
  await fixture([[], [movement(120)]], async (client, reads) => {
    const events = await Array.fromAsync(client.follow(run.run_id));
    assert.equal(reads(), 2);
    assert.ok(events.some((event) => event.entry?.text === "Reply"));
    assert.equal(events.at(-1).terminal, true);
  }, { turnOpen: false });
});

test("a new receipt hydrates prior native identity and retains an already completed current reply", async () => {
  const old = { turn_id: "old", thread_id: "thread" };
  const current = movement(130, { event_data: { turn_id: "new", thread_id: "thread" }, text: "Current reply" });
  for (const completed of [false, true]) await fixture([[current]], async (client, reads) => {
    const events = await Array.fromAsync(client.follow(run.run_id, receipt));
    assert.equal(reads(), completed ? 0 : 1);
    assert.equal(events.filter((event) => event.type === "status" && event.terminal).length, 1);
    assert.equal(events.at(-1).turnId, "new");
    assert.ok(events.some((event) => event.entry?.text === "Current reply"));
    if (!completed) assert.ok(events.some((event) => event.type === "request" && event.requestId === "approval"));
  }, { messages: [movement(90, { event_kind: "turn_started", terminal: false, event_data: old }),
    movement(120, { event_data: old }), ...(completed ? [current] : [movement(125,
      { event_kind: "approval", terminal: false, event_data: { request_id: "approval" } })])] });
});

test("submission, watch and restored idle projections wait past Stop ACK and reject stale closure providers", async () => {
  const ack = movement(110, { label: "turn.control.acknowledged", event_kind: "turn_control", event_data: {
    action: "stop", status: "acknowledged", turn_id: "turn", thread_id: "thread", detail: { exact_result_pending: true } } });
  const closure = movement(125, { label: "turn.interrupted.result", event_kind: "interrupted_result", event_data: {
    result_kind: "tool_result", correlation_id: "call", turn_id: "turn", thread_id: "thread", ok: true,
    turn_interrupted: true, returned_to_model: false, result_disposition: "durably_admitted_after_interrupt", source_artifact: { id: "result", content: Array(32).fill(10) } } });
  for (const mode of ["submission", "watch", "restored"]) {
    const originalReceipt = structuredClone(receipt);
    const batches = [[ack], [{ ...closure, movement_seq: 120, artifact_id: "wrong-provider", from_interface_id: "old-provider" }],
      [{ ...closure, movement_seq: 121, artifact_id: "old-turn", event_data: { ...closure.event_data, turn_id: "old" } }], [closure]];
    await fixture((index, request) => {
      assert.deepEqual(request.cursor, index ? { offset: 100 + index, seq: 100 + index } : receipt.cursor);
      return batches[index];
    }, async (client, reads) => {
      const events = await Array.fromAsync(client.follow(run.run_id, mode === "submission" ? receipt : { cursor: receipt.cursor }));
      assert.equal(reads(), 4);
      assert.ok(events.some((event) => event.type === "status" && event.state === "stopping" && !event.terminal));
      assert.equal(events.filter((event) => event.type === "status" && event.terminal).length, 1);
      assert.equal(events.at(-1).entry.kind, "interrupted_result");
      assert.equal(events.at(-1).interrupted, true);
      assert.deepEqual(receipt, originalReceipt);
    }, { turnOpen: false, messages: mode === "restored" ? [{ ...ack, id: "ack-history" }] : [] });
  }
});

test("noninteractive results retain only the selected turn reply through Stop, failure, replay and streaming", async () => {
  const Module = require("node:module"), file = path.resolve(__dirname, "../cli/index.cjs");
  const source = await fs.readFile(file, "utf8");
  const ack = movement(110, { event_kind: "turn_control", event_data: { action: "stop", status: "acknowledged",
    turn_id: "turn", thread_id: "thread", detail: { exact_result_pending: true } } });
  const closure = movement(120, { event_kind: "interrupted_result", event_data: { result_kind: "tool_result", correlation_id: "call",
    turn_id: "turn", thread_id: "thread", ok: true, turn_interrupted: true, returned_to_model: false,
    result_disposition: "durably_admitted_after_interrupt", source_artifact: { id: "result", content: Array(32).fill(10) } } });
  const oldReply = movement(90, { text: "Old baseline answer", event_data: { turn_id: "old", thread_id: "thread" } });
  for (const [result, expectedCode, command, restored, partial] of [
    ["stopped", 130, "watch", false, false], ["failed", 1, "watch", false, false], ["timeout", 124, "watch", false, false],
    ["stopped", 130, "watch", true, false], ["stopped", 130, "watch", true, true],
    ["stopped", 130, "send", false, true], ["failed", 1, "send", false, true], ["completed", 0, "send", false, true],
  ]) {
    const ending = result === "completed" ? movement(120, { text: "Current final", event_data: { turn_id: "turn", thread_id: "thread" } })
      : { ...closure, event_data: { ...closure.event_data, ok: result !== "failed" } };
    const fragments = ["Current ", "partial"].map((text, index) => ({
      source: "stream", run_id: run.run_id, interface_id: run.chat_interface_id,
      from_interface_id: run.provider_bound_interface_id, from_interface_role: "provider", recorded_at_unix_ms: 105 + index,
      source_movement_seq: 100, stream_id: "b".repeat(64), stream_seq: index + 1,
      byte_start_offset: index * 100, byte_end_offset: (index + 1) * 100, terminal: false, event_kind: "message_delta", text,
      event_data: { text, turn_id: "turn", thread_id: "thread", item_id: "current-reply" },
    }));
    const history = [oldReply, ...(restored ? [
      ...(partial ? [movement(105, { text: "Current partial", terminal: false, event_data: { turn_id: "turn", thread_id: "thread" } })] : []),
      movement(108, { text: "Other provider reply", terminal: false, from_interface_id: "other-provider", event_data: { turn_id: "turn", thread_id: "thread" } }),
      ack, ending,
    ] : [])];
    await fixture(async (index) => {
    if (!index) return [...(partial ? fragments : []), ...(result === "completed" ? [] : [ack])];
    if (result !== "timeout") assert.equal(index, 1);
    await new Promise((resolve) => setTimeout(resolve, 30));
    return result === "timeout" ? [] : [ending];
  }, async (client, reads) => {
    const unit = new Module(file, module); unit.filename = file; unit.paths = Module._nodeModulePaths(path.dirname(file));
    const nativeRequire = Module.createRequire(file);
    const observed = { ...client, async send(input) { assert.equal(input.runId, run.run_id); return receipt; }, async *follow(...args) {
      yield { type: "message", entry: { id: "commentary", role: "assistant", kind: "commentary", text: "I am working" } };
      yield { type: "delta", id: "commentary", role: "assistant", text: " on this." };
      yield* client.follow(...args);
    } };
    unit.require = (id) => id === "./environment.cjs" ? { createEnvironment: () => ({ close() {}, start() { throw new Error("No runtime startup allowed"); } }) }
      : id === "./client.cjs" ? { createClient: () => observed } : nativeRequire(id);
    unit._compile(source, file);
    let output = "";
    const destination = path.join(client.env.home, "last-message.txt");
    const code = await unit.exports.main([command, run.run_id, ...(command === "send" ? ["Continue"] : []), "--json", "--output-last-message", destination,
      ...(result === "timeout" ? ["--timeout", "0.01"] : [])], {
      stdin: { isTTY: false }, stdout: { write: (text) => { output += text; } }, stderr: { write() {} },
    });
    const rows = output.trim().split("\n").map(JSON.parse);
    assert.equal(code, expectedCode, JSON.stringify({ result, command, restored, partial, final: rows.at(-1) }));
    assert.equal(reads(), restored ? 0 : 2);
    if (!restored && result !== "completed") assert.ok(rows.some((row) => row.type === "status" && row.state === "stopping" && !row.terminal));
    assert.equal(rows.some((row) => row.type === "result" && row.status === "completed"), result === "completed");
    assert.equal(rows.at(-1).type, result === "timeout" ? "timeout" : "result");
    if (result !== "timeout") {
      assert.equal(rows.at(-1).status, result);
      const expected = result === "completed" ? "Current final" : partial ? "Current partial" : "";
      assert.equal(rows.at(-1).text, expected);
      assert.equal(await fs.readFile(destination, "utf8"), expected);
    } else await assert.rejects(fs.access(destination), { code: "ENOENT" });
  }, { turnOpen: !restored, messages: history });
  }
});

test("task deletion and follow-up refuse active work or an unresolved Stop even with an idle projection", async () => {
  const ack = movement(110, { event_kind: "turn_control", event_data: { action: "stop", status: "acknowledged",
    turn_id: "turn", thread_id: "thread", detail: { exact_result_pending: true } } });
  for (const turnOpen of [true, false]) await fixture([], async (client) => {
    await assert.rejects(client.deleteTask(run.run_id), /wait for it to settle/);
    await assert.rejects(client.send({ runId: run.run_id, text: "Do not send while stopping" }),
      (error) => error.code === (turnOpen ? "task_busy" : "task_stopping"));
  }, { turnOpen, messages: turnOpen ? [] : [ack] });
});

test("bound approvals accept terminal decisions and preserve exact CLI responses", async () => {
  const data = { request_id: "req", call_id: "call", kind: "application", detail: { approval_scope: "formed_app_target" } };
  const messages = [{ id: "question", event_kind: "approval", event_data: data, run_id: run.run_id, interface_id: run.chat_interface_id }];
  const captured = [];
  const boundary = new Error("Pure artifact boundary; no delivery attempted");
  await fixture([], async (client) => {
    for (const answer of [{ kind: "approval", decision: "accept" }, { kind: "approval", decision: "decline" }, { kind: "approval", decision: "cancel" }, { decision: "decline" }]) {
      await assert.rejects(client.respond(run.run_id, { kind: "approval", payload: data }, answer), (error) => error === boundary);
      assert.deepEqual(captured.at(-1).payload, { operator_approval_response: { request_id: "req", call_id: "call", decision: answer.decision } });
    }
    await assert.rejects(client.respond(run.run_id, "req", { kind: "approval", decision: "acceptForSession" }), /no supported decision/);
    assert.equal(captured.length, 4);
  }, { messages, authorize: (request) => { captured.push(request); throw boundary; } });
});

test("native permission refusal and MCP decline/cancel preserve typed backend responses", async () => {
  const permissions = { network: { enabled: true }, fileSystem: { read: ["/fixture/read"], write: ["/fixture/write"] } };
  const approval = { request_id: "permission", method: "item/permissions/requestApproval", detail: { permissions } };
  const elicitation = { request_id: "mcp", method: "mcpServer/elicitation/request", requested_schema: { type: "object", required: ["count"], properties: { count: { type: "integer" } } } };
  const messages = [["approval", approval], ["elicitation", elicitation]].map(([event_kind, event_data]) => ({
    id: event_data.request_id, event_kind, event_data, run_id: run.run_id, interface_id: run.chat_interface_id,
  }));
  const boundary = new Error("Pure artifact boundary; no delivery attempted"), captured = [];
  await fixture([], async (client) => {
    for (const decision of [{ permissions: {}, scope: "turn" }, { permissions, scope: "turn" }, { permissions, scope: "session" }]) {
      await assert.rejects(client.respond(run.run_id, "permission", { kind: "approval", decision }), (error) => error === boundary);
      assert.deepEqual(captured.at(-1).payload, { operator_approval_response: { request_id: "permission", decision } });
      assert.equal(captured.at(-1).label, "operator.approval.response");
    }
    for (const action of ["decline", "cancel"]) {
      const answer = { action, _meta: null };
      await assert.rejects(client.respond(run.run_id, "mcp", { kind: "elicitation", answer }), (error) => error === boundary);
      assert.deepEqual(captured.at(-1).payload, { operator_elicitation_response: { request_id: "mcp", answer } });
      assert.equal(captured.at(-1).label, "operator.elicitation.response");
    }
    assert.ok(captured.every((row) => row.runId === run.run_id && row.interfaceId === run.chat_interface_id));
  }, { messages, authorize: (request) => { captured.push(request); throw boundary; } });
});

test("noninteractive text is incremental, safe and complete; unsupported flags fail", () => {
  let output = "", errors = "";
  const emit = createEventRenderer({ stdout: { write: text => { output += text; } }, stderr: { write: text => { errors += text; } } });
  emit({ type: "message", entry: { id: "reply", role: "assistant", text: "Hel" } });
  emit({ type: "delta", id: "reply", role: "assistant", text: "lo" });
  emit({ type: "message", entry: { id: "reply", role: "assistant", text: "Hello" } });
  emit({ type: "message", entry: { id: "account", role: "assistant", kind: "event", text: "provider.account.updated" } });
  emit({ type: "activity", id: "tool", title: "Run\x1b[2J", status: "working" });
  assert.equal(output, "\nHello");
  assert.equal(errors, "Run · working\n");
  const failed = { type: "activity", id: "tool", title: "Code", status: "failed", script: "text(31); invalid();",
    output: "31", failure: "ReferenceError: invalid is not defined", notices: ["Output retained in the artifact."] };
  emit(failed);
  emit(failed);
  assert.equal(errors, "Run · working\nCode · failed\ntext(31); invalid();\n31\nReferenceError: invalid is not defined\nOutput retained in the artifact.\n");
  emit({ type: "activity", id: "exit", title: "Command", status: "exited", exitCode: 2 });
  assert.ok(errors.endsWith("Command · exited · exit 2\n"));
  assert.throws(() => argumentsFor(["setup", "--workspace-root", "/tmp"]), /Unknown option/);
  assert.throws(() => argumentsFor(["exec", "--timeout", "NaN"]), /between/);
});

test("CLI up forwards its explicit readiness deadline without starting a runtime", async () => {
  const Module = require("node:module"), file = path.resolve(__dirname, "../cli/index.cjs");
  const source = await fs.readFile(file, "utf8");
  const environmentFile = path.resolve(__dirname, "../cli/environment.cjs");
  const environment = new Module(environmentFile, module);
  environment.filename = environmentFile; environment.paths = Module._nodeModulePaths(path.dirname(environmentFile));
  const environmentRequire = Module.createRequire(environmentFile);
  const probeReached = new Error("validated startup reached controlled probe");
  environment.require = id => id === "../electron/runtimeHost.cjs" ? {
    createRuntimeHost: () => ({ async probe() { throw probeReached; } }),
  } : environmentRequire(id);
  environment._compile(await fs.readFile(environmentFile, "utf8"), environmentFile);
  await assert.rejects(environment.exports.start({}, { timeoutMs: 0 }), /positive integer/);
  for (const [flags, expected] of [[[], undefined], [["--timeout", "300"], 300000], [["--timeout", "1.25"], 1250], [["--timeout", "0"], undefined]]) {
    const unit = new Module(file, module); unit.filename = file; unit.paths = Module._nodeModulePaths(path.dirname(file));
    const nativeRequire = Module.createRequire(file);
    let started = 0, closed = 0, output = "";
    unit.require = (id) => id === "./environment.cjs" ? { createEnvironment: () => ({
      async start(options) {
        started++; assert.equal(options.timeoutMs, expected);
        await assert.rejects(environment.exports.start({}, options), error => error === probeReached);
        return { phase: "ready" };
      },
      close() { closed++; },
    }) } : id === "node:fs/promises" ? { ...fs, async access() {} } : nativeRequire(id);
    unit._compile(source, file);
    const code = await unit.exports.main(["up", "--json", ...flags], {
      stdin: { isTTY: false }, stdout: { isTTY: false, write: text => { output += text; } }, stderr: { write() {} },
    });
    assert.equal(code, 0, output);
    assert.equal(started, 1); assert.equal(closed, 1);
    assert.equal(JSON.parse(output).phase, "ready");
  }
});

test("CLI client commands fail closed upfront on an incomplete distribution and runtime commands forward cleanly", async () => {
  const { createEnvironment, requireDistribution } = require("../cli/environment.cjs");
  const { main } = require("../cli/index.cjs");
  const root = await fs.realpath(await fs.mkdtemp("/tmp/oct-cli-dist-"));
  try {
    const missingEnv = createEnvironment({ home: path.join(root, "home"), userHome: root, repoRoot: root, env: {} });
    assert.throws(() => requireDistribution(missingEnv), /build:cli.*--bundle/);
    const fakeApp = path.join(root, "app/build/Octopus.app/Contents/Resources/app/local-packages");
    await fs.mkdir(fakeApp, { recursive: true });
    if (process.platform === "darwin") {
      assert.throws(() => requireDistribution(missingEnv), /Octopus\.app/);
    }
    let stderr = "";
    const code = await main(["tasks", "--home", path.join(root, "home"), "--bundle", path.join(root, "missing-dist")], {
      stdin: { isTTY: false }, stdout: { write() {} }, stderr: { write: (text) => { stderr += text; } },
    });
    assert.equal(code, 1);
    assert.match(stderr, /Terminal distribution is incomplete/);

    const binDir = path.join(root, "dist/bin");
    await fs.mkdir(binDir, { recursive: true });
    const fakeBin = path.join(binDir, "octopus");
    await fs.writeFile(fakeBin, `#!${process.execPath}\nconsole.log("ok:" + process.argv[2]);\n`, { mode: 0o700 });
    const env = createEnvironment({ home: path.join(root, "home"), userHome: root, installationRoot: path.join(root, "dist"), env: {} });
    const result = await env.doctor();
    assert.equal(result.ok, true);
    assert.equal(result.stdout.trim(), "ok:doctor");
    env.close();
  } finally {
    await fs.rm(root, { recursive: true, force: true });
  }
});

test("CLI up --foreground validates flags, fails closed if running, forwards signals and preserves OCTOPUS_NODE on PATH", async () => {
  const { EventEmitter } = require("node:events");
  const Module = require("node:module");
  const indexFile = path.resolve(__dirname, "../cli/index.cjs");
  const envFile = path.resolve(__dirname, "../cli/environment.cjs");
  const indexSource = await fs.readFile(indexFile, "utf8");
  const envSource = await fs.readFile(envFile, "utf8");

  const loadIndex = (envStub) => {
    const unit = new Module(indexFile, module);
    unit.filename = indexFile;
    unit.paths = Module._nodeModulePaths(path.dirname(indexFile));
    const nativeRequire = Module.createRequire(indexFile);
    unit.require = (id) => id === "./environment.cjs" ? { createEnvironment: () => envStub }
      : id === "node:fs/promises" ? { ...fs, async access() {} } : nativeRequire(id);
    unit._compile(indexSource, indexFile);
    return unit.exports;
  };

  for (const [args, pattern] of [
    [["status", "--foreground"], /only supported with 'up'/],
    [["up", "--foreground", "--json"], /cannot be combined with --json/],
    [["up", "--foreground", "--timeout", "30"], /cannot be combined with --timeout/],
    [["up", "extra"], /does not accept positional arguments/],
  ]) {
    let stdout = "", stderr = "";
    const unit = loadIndex({ close() {}, async startForeground() { throw new Error("should not start"); } });
    const code = await unit.main(args, {
      stdin: { isTTY: false },
      stdout: { isTTY: false, write: (text) => { stdout += text; } },
      stderr: { write: (text) => { stderr += text; } },
    });
    assert.equal(code, 1, JSON.stringify({ args, stdout, stderr }));
    assert.match(stdout + stderr, pattern);
  }

  let fgCalls = 0;
  const okUnit = loadIndex({
    close() {},
    async startForeground() { fgCalls++; return { ok: true, exitCode: 0 }; },
  });
  assert.equal(await okUnit.main(["up", "--foreground"], {
    stdin: { isTTY: false }, stdout: { isTTY: false, write() {} }, stderr: { write() {} },
  }), 0);
  assert.equal(fgCalls, 1);

  const root = await fs.realpath(await fs.mkdtemp("/tmp/oct-cli-fg-"));
  try {
    const distRoot = path.join(root, "dist");
    for (const dir of [
      "bin",
      "registry/interface",
      "local-packages/registry/interface",
      "local-packages/packages",
      "local-packages/packs",
    ]) await fs.mkdir(path.join(distRoot, dir), { recursive: true });
    for (const marker of ["registry/.octopus-source.json", "local-packages/registry/.octopus-source.json"]) {
      await fs.writeFile(path.join(distRoot, marker), "{}\n");
    }
    const customNodeDir = path.join(root, "custom-node/bin");
    await fs.mkdir(customNodeDir, { recursive: true });
    const envDumpFile = path.join(root, "child-env.json");
    const readyFile = path.join(root, "child-ready");
    const fakeBin = path.join(distRoot, "bin/octopus");
    await fs.writeFile(fakeBin, [
      `#!${process.execPath}`,
      `const fs = require("node:fs");`,
      `fs.writeFileSync(${JSON.stringify(envDumpFile)}, JSON.stringify({ argv: process.argv.slice(2), PATH: process.env.PATH, OCTOPUS_HOME: process.env.OCTOPUS_HOME, OCTOPUS_MACHINE_CODEX_AUTH_FILE: process.env.OCTOPUS_MACHINE_CODEX_AUTH_FILE }));`,
      `process.on("SIGTERM", () => process.exit(0));`,
      `fs.writeFileSync(${JSON.stringify(readyFile)}, "ready");`,
      `setInterval(() => {}, 1000);`,
    ].join("\n") + "\n", { mode: 0o700 });

    let probeResult = { phase: "running", ready: true, pid: 4242, ownership: "external", error: null };
    const envMod = new Module(envFile, module);
    envMod.filename = envFile;
    envMod.paths = Module._nodeModulePaths(path.dirname(envFile));
    const envRequire = Module.createRequire(envFile);
    envMod.require = (id) => id === "../electron/runtimeHost.cjs"
      ? { createRuntimeHost: () => ({ async probe() { return probeResult; } }) }
      : envRequire(id);
    envMod._compile(envSource, envFile);

    const env = envMod.exports.createEnvironment({
      home: path.join(root, "home"),
      userHome: root,
      installationRoot: distRoot,
      env: {
        OCTOPUS_NODE: path.join(customNodeDir, "node"),
        PATH: "/usr/bin:/bin",
        ...(process.env.ELECTRON_RUN_AS_NODE ? { ELECTRON_RUN_AS_NODE: process.env.ELECTRON_RUN_AS_NODE } : {}),
      },
    });
    await assert.rejects(env.startForeground(), /already running.*pid 4242/);

    probeResult = { phase: "unmanaged", ready: false, pid: null, ownership: "none", error: null };
    const signalTarget = new EventEmitter();
    const running = env.startForeground({ signalTarget, stdio: ["ignore", "pipe", "pipe"] });
    const deadline = Date.now() + 10_000;
    while (Date.now() < deadline) {
      try { await fs.access(readyFile); break; } catch { await new Promise((r) => setTimeout(r, 25)); }
    }
    signalTarget.emit("SIGTERM");
    const result = await running;
    assert.equal(result.ok, true);
    assert.equal(result.exitCode, 0);
    assert.equal(result.forwardedSignal, "SIGTERM");
    assert.equal(signalTarget.listenerCount("SIGTERM"), 0);
    const dumped = JSON.parse(await fs.readFile(envDumpFile, "utf8"));
    assert.deepEqual(dumped.argv, ["up"]);
    assert.equal(dumped.OCTOPUS_HOME, path.join(root, "home"));
    assert.equal(dumped.OCTOPUS_MACHINE_CODEX_AUTH_FILE, path.join(root, ".codex/auth.json"));
    assert.ok(dumped.PATH.split(path.delimiter).includes(customNodeDir), dumped.PATH);
    assert.ok(dumped.PATH.split(path.delimiter).includes(path.dirname(process.execPath)), dumped.PATH);
    env.close();
  } finally {
    await fs.rm(root, { recursive: true, force: true });
  }
});
