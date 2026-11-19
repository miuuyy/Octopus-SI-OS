const assert = require("node:assert/strict");
const crypto = require("node:crypto");
const fs = require("node:fs");
const net = require("node:net");
const os = require("node:os");
const path = require("node:path");
const { bundles, buildInfo, swapBundles, rollbackBundles, control, readLedgerTurns, readTurnsInFlight, blockingTurns, newInterfaceFailures,
  packageAction, restartRuntime, parseArgs } = require("./safe-self-update.cjs");

const tmpBase = process.platform === "darwin" ? "/private/tmp" : os.tmpdir();
const roots = [];
function tempRoot() {
  const root = fs.mkdtempSync(path.join(tmpBase, "osu-test-"));
  roots.push(root);
  return root;
}

function fakeBundle(bundle, revision) {
  const app = path.join(bundle, "Contents", "Resources", "app");
  fs.mkdirSync(app, { recursive: true });
  fs.writeFileSync(path.join(app, "build-info.json"), JSON.stringify({ revision, modifiedSource: false }));
}

function fakeLayout({ live = "live", staging = "staging", prev = null } = {}) {
  const layout = bundles(path.join(tempRoot(), "build"));
  if (live) fakeBundle(layout.live, live);
  if (staging) fakeBundle(layout.staging, staging);
  if (prev) fakeBundle(layout.prev, prev);
  return layout;
}

function fakeRuntime(successorPhases, failuresByInstance = []) {
  const home = tempRoot(), directory = path.join(home, "state", "runtime-instance");
  fs.mkdirSync(directory, { recursive: true });
  const state = { started: 1, phase: "ready", requests: [], onRequest: null };
  const server = net.createServer((socket) => {
    socket.once("data", (chunk) => {
      const { request } = JSON.parse(String(chunk));
      state.requests.push(request);
      state.onRequest?.(request);
      socket.end(`${JSON.stringify({ outcome: "status", pid: 42, phase: state.phase, started_at_unix_ms: state.started, stopping: null,
        interface_failures: failuresByInstance[state.started - 1] ?? [] })}\n`);
      if (request === "restart") { state.phase = successorPhases[state.started - 1] ?? "ready"; state.started += 1; }
    });
  });
  return new Promise((resolve) => server.listen(path.join(directory, "control.sock"), () => resolve({ home, state, close: () => new Promise((done) => server.close(done)) })));
}

function fakeLedger(home) {
  const directory = path.join(home, "ledger"), blobs = path.join(directory, "blobs");
  fs.mkdirSync(blobs, { recursive: true });
  const file = path.join(directory, "movements.jsonl"), inputs = new Map();
  fs.writeFileSync(file, "");
  let seq = 0;
  const content = (value) => {
    const name = crypto.createHash("sha256").update(JSON.stringify(value)).digest("hex");
    fs.writeFileSync(path.join(blobs, name), JSON.stringify(value));
    return [...Buffer.from(name, "hex")];
  };
  const artifact = (label, ref) => ({ id: label + ":" + Buffer.from(ref).toString("hex"), content: ref });
  const row = (run, artifact, at, provider, kind = "InterfaceArrival") => {
    const [from, to] = kind === "InterfaceOutbound" ? ["worker", provider] : [provider, "worker"];
    fs.appendFileSync(file, JSON.stringify({ seq, kind, recorded_at_unix_ms: at,
      from: { Interface: from }, to: { Interface: to }, artifact,
      invocation_metadata: { runtime: { run, interface: provider, worker: "worker" } } }) + "\n");
    return seq++;
  };
  const providerEvent = (run, label, fields, at, provider) =>
    row(run, artifact(label, content({ label, ...fields })), at, provider);
  return {
    file,
    movement: (kind, at = 100) => row("fixture", null, at, "provider", kind),
    dispatch: (run, method, at = 100, { provider = "provider", manifest = false, kind } = {}) => {
      const ref = artifact("operator.message", content({ run, seq }));
      const input = { ref, seq: row(run, ref, at, provider, "InterfaceOutbound") };
      inputs.set(run, input);
      const label = "provider.transport.dispatch";
      const payload = content({ label, method, provider_interface: provider, source_movement_seq: input.seq });
      return row(run, artifact(label, manifest
        ? content({ label, payload_content_hash: Buffer.from(payload).toString("hex") }) : payload), at, provider, kind);
    },
    input: (run) => inputs.get(run),
    unreadableDispatch: (run, at = 100) => row(run, artifact("provider.transport.dispatch", Array(32).fill(7)), at, "provider"),
    event: providerEvent,
    close: (run, label, at = 100, fields = { in_reply_to: inputs.get(run).ref }, provider = "provider") =>
      providerEvent(run, label, { ...(label === "turn.error" ? { will_retry: false } : {}), ...fields }, at, provider),
    raw: (text) => fs.appendFileSync(file, text),
  };
}

function fakeProjectionBundle(layout, clientRuns, chats) {
  const electron = path.join(layout.live, "Contents", "Resources", "app", "electron");
  fs.mkdirSync(electron, { recursive: true });
  fs.writeFileSync(path.join(electron, "projectionReadContract.cjs"), "exports.projectionReadRequest = (method, params) => ({ method, params });");
  fs.writeFileSync(path.join(electron, "projection.cjs"), `exports.createProjectionStore = () => ({
    read: async ({ method }) => (method === "projection.operator.os.scope" ? { client_runs: ${JSON.stringify(clientRuns)} } : { chats: ${JSON.stringify(chats)} }),
    close: async () => {},
  });`);
}

const idle = async () => [];
const options = { timeoutMs: 1500 };

async function main() {
  let layout = fakeLayout();
  assert.equal(swapBundles(layout, { dryRun: true }).applied, false);
  assert.equal(buildInfo(layout.live).revision, "live");
  assert.equal(fs.existsSync(layout.prev), false);
  assert.equal(swapBundles(layout).revision, "staging");
  assert.equal(buildInfo(layout.prev).revision, "live");
  assert.equal(fs.existsSync(layout.staging), false);

  assert.throws(() => swapBundles(layout), /not a packaged Octopus bundle/);
  assert.equal(buildInfo(layout.live).revision, "staging");

  assert.equal(rollbackBundles(layout, { dryRun: true }).applied, false);
  assert.equal(rollbackBundles(layout).revision, "live");
  assert.equal(buildInfo(layout.failed).revision, "staging");
  assert.throws(() => rollbackBundles(layout), /not a packaged Octopus bundle/);
  assert.equal(buildInfo(layout.live).revision, "live");

  const chats = [{ run_id: "a", turn_open: true }, { run_id: "b", turn_open: false }, { run_id: "c", turn_open: true }];
  assert.deepEqual(blockingTurns(chats, "a").map((chat) => chat.run_id), ["c"]);
  assert.deepEqual(blockingTurns(chats, null).map((chat) => chat.run_id), ["a", "c"]);
  assert.deepEqual(blockingTurns(chats, ["a", null, "c"]), []);
  assert.deepEqual(blockingTurns(chats, ["a", "unrelated"]).map((chat) => chat.run_id), ["c"]);

  let home = tempRoot();
  await assert.rejects(readLedgerTurns(home), /movement ledger unavailable.*ENOENT/);
  let ledger = fakeLedger(home);
  ledger.movement("ArtifactMoved", 1);
  ledger.movement({ MarkOutcomeAdmitted: "task.summary.received" }, 1);
  const openedAt = ledger.dispatch("service_open", "turn/start", 100, { manifest: true });
  ledger.movement({ MarkOutcomeAdmitted: "tool.result.corresponds" });
  ledger.movement("VerificationFailed");
  ledger.movement("DeliveryRefused");
  ledger.dispatch("service_done", "turn/start");
  ledger.close("service_done", "turn.completed");
  ledger.dispatch("service_failed", "turn/start");
  ledger.close("service_failed", "turn.error");
  ledger.dispatch("service_interrupted", "turn/start");
  const interrupted = ledger.input("service_interrupted");
  ledger.dispatch("service_interrupted", "turn/interrupt");
  ledger.close("service_interrupted", "turn.interrupted.result", 100, { source_movement_seq: interrupted.seq });
  ledger.dispatch("service_answered", "turn/start");
  ledger.close("service_answered", "interface.artifact.output");
  ledger.dispatch("only_interrupt", "turn/interrupt");
  ledger.dispatch("outbound_only", "turn/start", 100, { kind: "InterfaceOutbound" });
  ledger.dispatch("before_this_runtime", "turn/start", 10);
  ledger.dispatch("client_chat", "turn/start");
  ledger.dispatch("service_reopened", "turn/start");
  ledger.close("service_reopened", "turn.completed");
  const reopenedAt = ledger.dispatch("service_reopened", "turn/start");
  assert.deepEqual(await readLedgerTurns(home, { sinceUnixMs: 50, skipRuns: ["client_chat"] }), [
    { run_id: "service_open", turn_open: true, turn_state: "working", source: "ledger", opened_movement_seq: openedAt },
    { run_id: "service_reopened", turn_open: true, turn_state: "working", source: "ledger", opened_movement_seq: reopenedAt },
  ]);
  assert.deepEqual((await readLedgerTurns(home)).map((turn) => turn.run_id).sort(),
    ["before_this_runtime", "client_chat", "service_open", "service_reopened"]);

  for (const kind of [
    "MarkOutcomeAdmitted", "UnknownMovement", {}, { Unexpected: "mark" },
    { MarkOutcomeAdmitted: "" }, { MarkOutcomeAdmitted: null }, { MarkOutcomeAdmitted: 1 },
    { MarkOutcomeAdmitted: ["mark"] }, { MarkOutcomeAdmitted: { id: "mark" } },
    { MarkOutcomeAdmitted: "mark", extra: 1 }, { ArtifactMoved: null }, [{ MarkOutcomeAdmitted: "mark" }],
  ]) {
    const malformedHome = tempRoot();
    fakeLedger(malformedHome).movement(kind, 1);
    await assert.rejects(readLedgerTurns(malformedHome, { sinceUnixMs: 50 }),
      /invalid or non-contiguous movement ledger row after sequence -1/);
  }

  layout = fakeLayout();
  fakeProjectionBundle(layout, [{ run_id: "client_chat" }, { run_id: "client_idle" }],
    [{ run_id: "client_chat", turn_open: true, turn_state: "working" }, { run_id: "client_idle", turn_open: false, turn_state: "idle" },
      { run_id: "outside_scope", turn_open: true, turn_state: "working" }]);
  const inFlight = await readTurnsInFlight(layout.live, home, { sinceUnixMs: 50 });
  assert.deepEqual(inFlight.map((turn) => [turn.run_id, turn.source, turn.turn_open]), [
    ["client_chat", "client", true], ["client_idle", "client", false],
    ["service_open", "ledger", true], ["service_reopened", "ledger", true]]);
  assert.deepEqual(blockingTurns(inFlight, "client_chat").map((turn) => turn.run_id), ["service_open", "service_reopened"]);

  home = tempRoot();
  ledger = fakeLedger(home);
  ledger.dispatch("native", "turn/start");
  const reply = ledger.input("native").ref;
  ledger.event("native", "turn.started", { in_reply_to: reply, threadId: "thread", turn: { id: "current" } }, 100, "provider");
  ledger.close("native", "turn.completed", 100, { in_reply_to: reply, threadId: "thread", turn: { id: "older" } });
  await assert.rejects(readLedgerTurns(home), /ambiguous turn closure/);
  ledger.close("native", "turn.completed", 100, { threadId: "thread", turn: { id: "current" } });
  assert.deepEqual(await readLedgerTurns(home), []);
  ledger.dispatch("native", "turn/start");
  ledger.close("native", "interface.artifact.output", 100, { in_reply_to: reply });
  await assert.rejects(readLedgerTurns(home), /ambiguous turn closure/);

  home = tempRoot();
  ledger = fakeLedger(home);
  const dispatch = ledger.dispatch("provider_bound", "turn/start");
  ledger.close("provider_bound", "turn.completed", 100, undefined, "another_provider");
  assert.equal((await readLedgerTurns(home))[0].opened_movement_seq, dispatch);
  ledger.close("provider_bound", "interface.artifact.output", 100, {});
  await assert.rejects(readLedgerTurns(home), /ambiguous turn closure/);

  const failure = (name, error = "life exited") => ({ interface: name, error });
  assert.deepEqual(newInterfaceFailures({ interface_failures: [] }, { interface_failures: [] }), []);
  assert.deepEqual(newInterfaceFailures({ interface_failures: [failure("a")] }, { interface_failures: [failure("a", "another error")] }), []);
  assert.deepEqual(newInterfaceFailures({ interface_failures: [failure("a")] }, { interface_failures: [failure("b")] }), ["b"]);
  assert.deepEqual(newInterfaceFailures({ interface_failures: [failure("a"), failure("b")] }, { interface_failures: [failure("c")] }), ["c"]);
  assert.deepEqual(newInterfaceFailures({ interface_failures: [failure("a"), failure("b")] }, { interface_failures: [failure("b")] }), []);
  assert.deepEqual(newInterfaceFailures({}, { interface_failures: [failure("a")] }), ["a"]);

  assert.equal(packageAction({ id: "x", update_available: false }), "current");
  assert.equal(packageAction({ id: "x", update_available: true, local_changes: [] }), "repair");
  assert.equal(packageAction({ id: "x", update_available: true, local_changes: [{}] }), "review_local_changes");
  assert.equal(packageAction({ id: "world_operator_chat", update_available: true }), "repair_with_client_restart");
  assert.equal(packageAction({ id: "x", update_available: true, error: "stale" }), "blocked");

  assert.deepEqual(parseArgs(["restart", "--swap", "--caller-run=run_1"]), { positional: ["restart"], options: { swap: true, callerRun: "run_1" } });
  assert.deepEqual(parseArgs(["restart", "--allow-in-flight-run", "run_1", "--allow-in-flight-run=run_2", "--dry-run"]),
    { positional: ["restart"], options: { allowInFlightRun: ["run_1", "run_2"], dryRun: true } });
  assert.throws(() => parseArgs(["restart", "--allow-in-flight-run"]), /requires a run id/);
  assert.throws(() => parseArgs(["restart", "--allow-in-flight-run", "--swap"]), /requires a run id/);
  assert.equal((await control(tempRoot(), "status", 500)).outcome, "unreachable");

  layout = fakeLayout();
  let runtime = await fakeRuntime([]);
  let result = await restartRuntime(layout, runtime.home, { ...options, swap: true, callerRun: "me",
    readTurns: async () => [{ run_id: "me", turn_open: true }, { run_id: "other", turn_open: true }] });
  assert.equal(result.refused, "turns_in_flight");
  assert.deepEqual(result.blocking.map((chat) => chat.run_id), ["other"]);
  assert.deepEqual(runtime.state.requests, ["status"]);
  assert.equal(buildInfo(layout.live).revision, "live");

  fakeProjectionBundle(layout, [{ run_id: "me" }], [{ run_id: "me", turn_open: true, turn_state: "working" }]);
  ledger = fakeLedger(runtime.home);
  ledger.dispatch("sensor_triggered", "turn/start", 5);
  ledger.dispatch("memory_service", "turn/start", 5);
  ledger.dispatch("stopped_by_an_earlier_restart", "turn/start", 0);
  runtime.state.requests.length = 0;
  result = await restartRuntime(layout, runtime.home, { ...options, swap: true, callerRun: "me" });
  assert.equal(result.refused, "turns_in_flight");
  assert.deepEqual(result.blocking.map((turn) => [turn.run_id, turn.source]), [["sensor_triggered", "ledger"], ["memory_service", "ledger"]]);
  result = await restartRuntime(layout, runtime.home, { ...options, swap: true, callerRun: "me", allowInFlightRuns: ["sensor_triggered"] });
  assert.equal(result.refused, "turns_in_flight");
  assert.deepEqual(result.blocking.map((turn) => turn.run_id), ["memory_service"]);
  assert.deepEqual(runtime.state.requests, ["status", "status"]);
  assert.equal(buildInfo(layout.live).revision, "live");
  result = await restartRuntime(layout, runtime.home, { ...options, swap: true, dryRun: true, callerRun: "me",
    allowInFlightRuns: ["sensor_triggered", "memory_service"] });
  assert.equal(result.ok, true);
  assert.deepEqual(result.blocking, []);
  result = await restartRuntime(layout, runtime.home, { ...options, swap: true, dryRun: true, allowInFlightRuns: ["sensor_triggered", "memory_service"] });
  assert.deepEqual(result.blocking.map((turn) => turn.run_id), ["me"]);
  assert.equal(runtime.state.requests.includes("restart"), false);

  result = await restartRuntime(layout, runtime.home, { ...options, swap: true, dryRun: true, readTurns: idle });
  assert.equal(result.ok, true);
  assert.equal(result.applied, false);
  assert.equal(runtime.state.requests.includes("restart"), false);
  assert.equal(buildInfo(layout.live).revision, "live");

  result = await restartRuntime(layout, runtime.home, { ...options, swap: true, readTurns: idle });
  assert.equal(result.ok, true);
  assert.equal(result.revision, "staging");
  assert.equal(result.runtime.started_at_unix_ms, 2);
  assert.equal(buildInfo(layout.prev).revision, "live");
  await runtime.close();

  for (const [damage, expected] of [
    [() => {}, /ENOENT/],
    [(home) => fs.mkdirSync(path.join(home, "ledger/movements.jsonl"), { recursive: true }), /not a regular file/],
    ...(process.getuid?.() > 0 ? [[(home) => fs.chmodSync(fakeLedger(home).file, 0), /movement ledger unreadable.*EACCES/]] : []),
    [(home) => fakeLedger(home).raw('{"seq":0'), /torn movement ledger/],
    [(home) => fakeLedger(home).raw('{"seq":0\n'), /malformed movement ledger/],
    [(home) => fakeLedger(home).movement({ MarkOutcomeAdmitted: "" }), /invalid or non-contiguous movement ledger/],
    [(home) => {
      const ledger = fakeLedger(home);
      ledger.movement({ MarkOutcomeAdmitted: "tool.result.corresponds" });
      ledger.raw('{"seq":2,"kind":"ArtifactMoved","recorded_at_unix_ms":100}\n');
    }, /non-contiguous movement ledger row after sequence 0/],
    [(home) => fakeLedger(home).unreadableDispatch("unreadable"), /unreadable provider.transport.dispatch/],
    [(home) => {
      const ledger = fakeLedger(home);
      ledger.dispatch("service", "turn/start");
      ledger.close("service", "turn.completed", 100, {});
    }, /ambiguous turn closure/],
  ]) {
    layout = fakeLayout();
    runtime = await fakeRuntime([]);
    try {
      fakeProjectionBundle(layout, [], []);
      damage(runtime.home);
      result = await restartRuntime(layout, runtime.home, { ...options, swap: true, force: true });
      assert.equal(result.refused, "turn_evidence_untrusted");
      assert.match(result.error, expected);
      assert.deepEqual(runtime.state.requests, ["status"]);
      assert.equal(buildInfo(layout.live).revision, "live");
      assert.equal(buildInfo(layout.staging).revision, "staging");
      assert.equal(fs.existsSync(layout.prev), false);
    } finally { await runtime.close(); }
  }

  for (const [scenario, expected] of [
    ["late", "turns_in_flight"], ["late_after_scan", "turn_frontier_changed"], ["changed", "runtime_changed"], ["unreachable", "runtime_unreachable"],
  ]) {
    layout = fakeLayout({ prev: "rollback-point" });
    runtime = await fakeRuntime([]);
    let closed = false;
    try {
      fakeProjectionBundle(layout, [], []);
      const ledger = fakeLedger(runtime.home);
      let observations = 0;
      runtime.state.onRequest = (request) => {
        if (request !== "status") return;
        ++observations;
        if (observations === 2 && scenario === "late") ledger.dispatch("late_work", "turn/start");
        if (observations === 3 && scenario === "late_after_scan") ledger.dispatch("late_work", "turn/start");
        if (observations === 3 && scenario === "changed") runtime.state.started += 1;
      };
      const readTurns = scenario === "unreachable" ? async (...args) => {
        const turns = await readTurnsInFlight(...args);
        await runtime.close();
        closed = true;
        return turns;
      } : undefined;
      result = await restartRuntime(layout, runtime.home, { ...options, swap: true, force: true, readTurns });
      assert.equal(result.refused, expected);
      assert.equal(runtime.state.requests.includes("restart"), false);
      assert.equal(buildInfo(layout.live).revision, "live");
      assert.equal(buildInfo(layout.staging).revision, "staging");
      assert.equal(buildInfo(layout.prev).revision, "rollback-point");
    } finally { if (!closed) await runtime.close(); }
  }

  layout = fakeLayout();
  runtime = await fakeRuntime([]);
  try {
    fakeProjectionBundle(layout, [], []);
    ledger = fakeLedger(runtime.home);
    ledger.dispatch("finished", "turn/start");
    ledger.close("finished", "turn.completed");
    result = await restartRuntime(layout, runtime.home, { ...options, swap: true });
    assert.equal(result.ok, true);
    assert.deepEqual(runtime.state.requests, ["status", "status", "status", "restart", "status"]);
    assert.equal(buildInfo(layout.live).revision, "staging");
    assert.equal(buildInfo(layout.prev).revision, "live");
  } finally { await runtime.close(); }

  layout = fakeLayout();
  runtime = await fakeRuntime(["starting", "ready"]);
  result = await restartRuntime(layout, runtime.home, { ...options, swap: true, readTurns: idle });
  assert.equal(result.ok, false);
  assert.equal(result.rolled_back, true);
  assert.equal(result.recovered, true);
  assert.equal(runtime.state.requests.filter((request) => request === "restart").length, 2);
  assert.equal(buildInfo(layout.live).revision, "live");
  assert.equal(buildInfo(layout.failed).revision, "staging");
  await runtime.close();

  layout = fakeLayout();
  runtime = await fakeRuntime([], [[failure("world_a")], [failure("world_b")], [failure("world_a")]]);
  result = await restartRuntime(layout, runtime.home, { ...options, swap: true, readTurns: idle });
  assert.equal(result.ok, false);
  assert.deepEqual(result.new_interface_failures, ["world_b"]);
  assert.equal(result.rolled_back, true);
  assert.equal(result.recovered, true);
  assert.equal(buildInfo(layout.live).revision, "live");
  assert.equal(buildInfo(layout.failed).revision, "staging");
  await runtime.close();

  layout = fakeLayout();
  runtime = await fakeRuntime([], [[failure("world_a")], [failure("world_b")]]);
  result = await restartRuntime(layout, runtime.home, { ...options, rollback: false, readTurns: idle });
  assert.equal(result.ok, false);
  assert.equal(result.rolled_back, false);
  assert.deepEqual(result.new_interface_failures, ["world_b"]);
  await runtime.close();

  layout = fakeLayout();
  runtime = await fakeRuntime([], [[failure("world_a"), failure("world_b")], [failure("world_a", "still failing")]]);
  result = await restartRuntime(layout, runtime.home, { ...options, swap: true, readTurns: idle });
  assert.equal(result.ok, true);
  assert.equal(result.revision, "staging");
  await runtime.close();

  layout = fakeLayout();
  await assert.rejects(restartRuntime(layout, tempRoot(), { ...options, swap: true, readTurns: idle }), /no runtime answers/);
  assert.equal(buildInfo(layout.live).revision, "live");
  console.log("safe self-update checks passed");
}

main().finally(() => { for (const root of roots) fs.rmSync(root, { recursive: true, force: true }); })
  .catch((error) => { console.error(error); process.exitCode = 1; });
