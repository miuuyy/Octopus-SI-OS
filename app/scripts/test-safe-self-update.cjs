const assert = require("node:assert/strict");
const crypto = require("node:crypto");
const fs = require("node:fs");
const net = require("node:net");
const os = require("node:os");
const path = require("node:path");
const { spawnSync } = require("node:child_process");
const { bundles, buildInfo, buildStaging, verifyStaging, isValidRuntimeUsageOutput, checkCliLayoutParity, status,
  checkBuildDiskSpace, resolveMinFreeGb, pruneBuildArtifacts,
  swapBundles, rollbackBundles, control, readLedgerTurns, readTurnsInFlight, blockingTurns, newInterfaceFailures,
  packageAction, availablePackages, installPackages, main: updaterMain, restartRuntime, parseArgs, resolveQuietWaitOptions, resolveRestartTimeoutMs,
  readRuntimeInstanceState, parseLaunchdService, inspectLaunchdService,
  parseSystemctlShow, parseSystemctlExecStart, parseSystemctlEnvironment, inspectSystemdService,
  isSystemdUnitFailedOrCrashLooping, terminateProcess, runtimeEnv } = require("./safe-self-update.cjs");

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
  await require("./support/package-transfer-tests.cjs")(fakeLayout, tempRoot);
  {
    const bundle = fakeLayout().live, home = tempRoot();
    const app = path.join(bundle, "Contents", "Resources", "app");
    const electron = path.join(app, "electron"), library = path.join(app, "local-packages");
    fs.mkdirSync(electron, { recursive: true });
    fs.mkdirSync(path.join(home, "registry"), { recursive: true });
    fs.writeFileSync(path.join(electron, "localPackages.cjs"), `module.exports = require(${JSON.stringify(path.resolve(__dirname, "../electron/localPackages.cjs"))});`);
    const publication = process.argv.includes("--native-publication")
      ? `require(${JSON.stringify(path.resolve(__dirname, "support/native-publication.cjs"))}).nativePublication()`
      : `require(${JSON.stringify(path.resolve(__dirname, "fixturePublication.cjs"))}).fixtureDirectoryPublisher(${JSON.stringify(home)})`;
    fs.writeFileSync(path.join(electron, "packageStorage.cjs"), `exports.createPackageStoragePublisher = () => {
      const publisher = ${publication};
      return ${process.argv.includes("--native-publication") ? "publisher" : `{ ...publisher, publishFiles: require(${JSON.stringify(path.resolve(__dirname, "fixturePublication.cjs"))}).fixtureFilePublisher(${JSON.stringify(home)}) }`};
    };`);
    const json = (file, value) => { fs.mkdirSync(path.dirname(file), { recursive: true }); fs.writeFileSync(file, JSON.stringify(value)); };
    for (const id of ["optional_base", "optional_sensor"]) {
      json(path.join(library, "registry", "interface", id, "interface.json"), {
        id, interface_type: "world", role: "sensor", direction: "inbound", source_names: [id],
      });
      json(path.join(library, "packages", id + ".json"), {
        schema: "octopus.local_package", schema_version: 1, id, version: "1.0.0",
        product_kind: "application", product_id: id, release_status: "optional",
        display: { name: id, description: "Optional fixture", developer: "Octopus", category: "sensor" },
        members: [{ registry_kind: "interface", owner: id, primary: true }],
        ...(id === "optional_sensor" ? { required_packages: ["optional_base"] } : {}),
      });
    }
    const available = await availablePackages(bundle, home);
    assert.equal(available.ok, true);
    assert.deepEqual(available.packages.map(p => p.id).sort(), ["optional_base", "optional_sensor"]);
    const plan = await installPackages(bundle, home, ["optional_sensor"], { dryRun: true });
    assert.deepEqual(plan.install, ["optional_base", "optional_sensor"]);
    assert.equal(plan.applied, false);
    assert.deepEqual(fs.readdirSync(path.join(home, "registry")), []);
    assert.equal(fs.existsSync(path.join(home, "client")), false);
    await assert.rejects(installPackages(bundle, home, ["missing"], { dryRun: true }), /Unknown package.*packages available/);
    await assert.rejects(installPackages(bundle, home, ["optional_sensor", "missing"]), /Unknown package/);
    await assert.rejects(installPackages(bundle, home, ["../optional_sensor"]), /Invalid package id/);
    await assert.rejects(installPackages(bundle, home, []), /at least one/);
    assert.equal(fs.existsSync(path.join(home, "client")), false);
    const result = await installPackages(bundle, home, ["optional_sensor"]);
    assert.equal(result.result.installed, true);
    assert.equal(result.result.changed, true);
    const installed = await availablePackages(bundle, home);
    assert.ok(installed.packages.every(p => p.installed && p.receipt_state === "valid"));
    const config = path.join(home, "registry", "interface", "optional_sensor", "environment.json");
    fs.writeFileSync(config, '{"TOKEN":"fixture-only"}');
    const receipt = path.join(home, "client", "package-state", "receipts", "optional_sensor.json");
    const beforeReceipt = fs.readFileSync(receipt, "utf8");
    const again = await installPackages(bundle, home, ["optional_sensor"]);
    assert.equal(again.result.changed, false);
    assert.equal(fs.readFileSync(config, "utf8"), '{"TOKEN":"fixture-only"}');
    assert.equal(fs.readFileSync(receipt, "utf8"), beforeReceipt);
    const args = ["--target", "app", "--build-root", path.dirname(bundle), "--home", home];
    await assert.rejects(updaterMain(["packages", "install", "optional_sensor", "--dry-rnu", ...args]), /Unknown packages option/);
    await assert.rejects(updaterMain(["packages", "install", "optional_sensor", "--dry-run=false", ...args]), /boolean flag/);
    const cliPlan = await updaterMain(["packages", "install", "optional_sensor", "--dry-run", "--json", ...args]);
    assert.deepEqual(cliPlan.install, []);
    const cliBundle = path.join(tempRoot(), "cli");
    fs.mkdirSync(path.join(cliBundle, "runtime"), { recursive: true });
    fs.cpSync(library, path.join(cliBundle, "runtime", "local-packages"), { recursive: true });
    fs.cpSync(electron, path.join(cliBundle, "electron"), { recursive: true });
    json(path.join(cliBundle, "runtime", "runtime-build-receipt.json"), { schema: "octopus.cli_runtime_distribution" });
    const linuxPlan = await updaterMain(["packages", "install", "optional_sensor", "--dry-run", "--platform", "linux", "--install-dir", cliBundle, "--home", home]);
    assert.deepEqual(linuxPlan.install, []);
    assert.deepEqual(linuxPlan.preserved, ["optional_base", "optional_sensor"]);
    const failed = spawnSync(process.execPath, [path.join(__dirname, "safe-self-update.cjs"), "packages", "install", "missing", "--json", ...args], { encoding: "utf8" });
    assert.equal(failed.status, 2);
    assert.match(JSON.parse(failed.stdout).error, /Unknown package/);
  }
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

  assert.deepEqual(resolveQuietWaitOptions(parseArgs(["restart", "--wait-quiet"]).options), { waitQuietMs: 300_000, quietWindowMs: 10_000 });
  assert.deepEqual(resolveQuietWaitOptions(parseArgs(["restart", "--wait-quiet=45", "--quiet-for=5"]).options), { waitQuietMs: 45_000, quietWindowMs: 5_000 });
  assert.throws(() => resolveQuietWaitOptions(parseArgs(["restart", "--quiet-for=5"]).options), /requires --wait-quiet/);
  assert.throws(() => resolveQuietWaitOptions(parseArgs(["restart", "--wait-quiet=0"]).options), /between 1 and 3600/);

  layout = fakeLayout();
  runtime = await fakeRuntime([]);
  try {
    fakeProjectionBundle(layout, [], []);
    ledger = fakeLedger(runtime.home);
    ledger.dispatch("memory_observer", "turn/start", 5);
    setTimeout(() => ledger.close("memory_observer", "turn.completed", 6), 40);
    result = await restartRuntime(layout, runtime.home, { ...options, swap: true, waitQuietMs: 1000, quietWindowMs: 30, pollIntervalMs: 15 });
    assert.equal(result.ok, true);
    assert.equal(result.revision, "staging");
    assert.ok(result.attempts >= 2);
    assert.equal(buildInfo(layout.live).revision, "staging");
  } finally { await runtime.close(); }

  layout = fakeLayout();
  runtime = await fakeRuntime([]);
  try {
    fakeProjectionBundle(layout, [], []);
    ledger = fakeLedger(runtime.home);
    ledger.dispatch("stuck_observer", "turn/start", 5);
    result = await restartRuntime(layout, runtime.home, { ...options, swap: true, waitQuietMs: 80, quietWindowMs: 20, pollIntervalMs: 15 });
    assert.equal(result.ok, false);
    assert.equal(result.refused, "turns_in_flight");
    assert.equal(result.timed_out, true);
    assert.equal(buildInfo(layout.live).revision, "live");
  } finally { await runtime.close(); }

  layout = fakeLayout();
  runtime = await fakeRuntime([]);
  try {
    fakeProjectionBundle(layout, [], []);
    fakeLedger(runtime.home).raw('{"seq":0\n');
    result = await restartRuntime(layout, runtime.home, { ...options, swap: true, waitQuietMs: 500, quietWindowMs: 20, pollIntervalMs: 15 });
    assert.equal(result.ok, false);
    assert.equal(result.refused, "turn_evidence_untrusted");
    assert.equal(result.timed_out, undefined);
    assert.equal(result.attempts, 1);
  } finally { await runtime.close(); }

  layout = fakeLayout();
  await assert.rejects(restartRuntime(layout, tempRoot(), { ...options, swap: true, readTurns: idle }), /no runtime answers/);
  assert.equal(buildInfo(layout.live).revision, "live");

  // Rollback recovery helper contracts: state parsing, launchd ownership guard, and SIGTERM -> SIGKILL escalation.
  const parsedLaunchd = parseLaunchdService([
    "gui/501/app.octopus.runtime = {",
    "\tstate = running",
    "\tprogram = /opt/Octopus.app/Contents/Resources/app/bin/octopus",
    "\tenvironment = {",
    "\t\tOCTOPUS_HOME => /var/lib/octopus-home",
    "\t}",
    "\tpid = 8123",
    "\tresource coalition = {",
    "\t\tstate = active",
    "\t}",
    "}",
  ].join("\n"));
  assert.deepEqual(parsedLaunchd, {
    state: "running",
    program: "/opt/Octopus.app/Contents/Resources/app/bin/octopus",
    octopusHome: "/var/lib/octopus-home",
    pid: 8123,
  });

  layout = fakeLayout();
  home = tempRoot();
  const liveBin = path.join(layout.live, "Contents", "Resources", "app", "bin", "octopus");
  assert.equal(readRuntimeInstanceState(home), null);
  fs.mkdirSync(path.join(home, "state", "runtime-instance"), { recursive: true });
  fs.writeFileSync(path.join(home, "state", "runtime-instance", "state.json"), JSON.stringify({
    schema_version: 3, pid: 9001, phase: "starting", started_at_unix_ms: 55, updated_at_unix_ms: 55, launch_owner: null,
  }));
  assert.deepEqual(readRuntimeInstanceState(home), { pid: 9001, phase: "starting", started_at_unix_ms: 55 });

  assert.equal(inspectLaunchdService(layout, home, { platform: "linux", uid: 501 }).loaded, false);
  assert.equal(inspectLaunchdService(layout, home, {
    platform: "darwin", uid: 501, launchctl: () => ({ status: 113, stdout: "", stderr: "Could not find service" }),
  }).loaded, false);
  const matchedService = inspectLaunchdService(layout, home, {
    platform: "darwin", uid: 501,
    launchctl: () => ({ status: 0, stdout: `\tstate = waiting\n\tprogram = ${liveBin}\n\t\tOCTOPUS_HOME => ${home}\n\tpid = 9001\n`, stderr: "" }),
  });
  assert.equal(matchedService.loaded, true);
  assert.equal(matchedService.matchesTarget, true);
  assert.equal(matchedService.target, "gui/501/app.octopus.runtime");
  assert.equal(matchedService.pid, 9001);
  const wrongHomeService = inspectLaunchdService(layout, home, {
    platform: "darwin", uid: 501,
    launchctl: () => ({ status: 0, stdout: `\tstate = running\n\tprogram = ${liveBin}\n\t\tOCTOPUS_HOME => ${tempRoot()}\n`, stderr: "" }),
  });
  assert.equal(wrongHomeService.loaded, true);
  assert.equal(wrongHomeService.matchesTarget, false);
  const wrongBinService = inspectLaunchdService(layout, home, {
    platform: "darwin", uid: 501,
    launchctl: () => ({ status: 0, stdout: `\tstate = running\n\tprogram = /other/Octopus.app/Contents/Resources/app/bin/octopus\n\t\tOCTOPUS_HOME => ${home}\n`, stderr: "" }),
  });
  assert.equal(wrongBinService.loaded, true);
  assert.equal(wrongBinService.matchesTarget, false);

  {
    let alive = true;
    const sent = [];
    const termResult = await terminateProcess(7777, {
      isProcessAlive: () => alive,
      sendSignal: (pid, signal) => { sent.push([pid, signal]); if (signal === "SIGTERM") alive = false; return { delivered: true }; },
      graceMs: 80, pollMs: 10,
    });
    assert.deepEqual(sent, [[7777, "SIGTERM"]]);
    assert.equal(termResult.terminated, true);
    assert.deepEqual(termResult.signals, ["SIGTERM"]);
  }
  {
    let alive = true;
    const sent = [];
    const killResult = await terminateProcess(7778, {
      isProcessAlive: () => alive,
      sendSignal: (pid, signal) => { sent.push([pid, signal]); if (signal === "SIGKILL") alive = false; return { delivered: true }; },
      graceMs: 35, pollMs: 10,
    });
    assert.deepEqual(sent, [[7778, "SIGTERM"], [7778, "SIGKILL"]]);
    assert.equal(killResult.terminated, true);
    assert.deepEqual(killResult.signals, ["SIGTERM", "SIGKILL"]);
  }
  {
    const selfSkip = await terminateProcess(process.pid, {
      isProcessAlive: () => { throw new Error("must not probe self"); },
      sendSignal: () => { throw new Error("must not signal self"); },
    });
    assert.equal(selfSkip.skipped, "invalid_pid");
  }

  async function bindControlServer(homeDir, handler) {
    const directory = path.join(homeDir, "state", "runtime-instance");
    fs.mkdirSync(directory, { recursive: true });
    const sock = path.join(directory, "control.sock");
    fs.rmSync(sock, { force: true });
    const server = net.createServer((socket) => {
      socket.once("data", (chunk) => {
        const { request } = JSON.parse(String(chunk));
        handler(request, socket);
      });
    });
    await new Promise((resolve) => server.listen(sock, resolve));
    return { close: () => new Promise((done) => server.close(done)) };
  }

  // Scenario A: hung successor unreachable on control.sock, exits on SIGTERM, recovered via launchctl kickstart -k.
  for (const ignoreTerm of [false, true]) {
    layout = fakeLayout();
    const targetHome = tempRoot();
    const targetBin = path.join(layout.live, "Contents", "Resources", "app", "bin", "octopus");
    const hungPid = 6100 + (ignoreTerm ? 1 : 0);
    let hungAlive = true;
    const signals = [];
    const launchctlCalls = [];
    let recoveredServer = null;
    let liveRevisionAtSignal = null;
    let liveRevisionAtKickstart = null;
    const initialServer = await bindControlServer(targetHome, (request, socket) => {
      socket.end(`${JSON.stringify({ outcome: "status", pid: hungPid, phase: "ready", started_at_unix_ms: 10, stopping: null, interface_failures: [] })}\n`);
      if (request === "restart") {
        // Simulate new binary exec()ing with same PID, writing state.json, and hanging before binding control.sock.
        initialServer.close();
        fs.rmSync(path.join(targetHome, "state", "runtime-instance", "control.sock"), { force: true });
        fs.writeFileSync(path.join(targetHome, "state", "runtime-instance", "state.json"), JSON.stringify({
          schema_version: 3, pid: hungPid, phase: "starting", started_at_unix_ms: 20, updated_at_unix_ms: 20, launch_owner: null,
        }));
      }
    });
    try {
      result = await restartRuntime(layout, targetHome, {
        timeoutMs: 120, recoveryTimeoutMs: 1500, statusPollMs: 20, terminateGraceMs: 40, terminatePollMs: 10,
        swap: true, readTurns: idle, platform: "darwin", uid: 501,
        isProcessAlive: (pid) => pid === hungPid && hungAlive,
        sendSignal: (pid, signal) => {
          liveRevisionAtSignal = buildInfo(layout.live)?.revision ?? null;
          signals.push([pid, signal]);
          if (pid === hungPid && (!ignoreTerm || signal === "SIGKILL")) hungAlive = false;
          return { delivered: true };
        },
        launchctl: (args) => {
          launchctlCalls.push(args);
          if (args[0] === "print") {
            return { status: 0, stdout: `\tstate = running\n\tprogram = ${targetBin}\n\t\tOCTOPUS_HOME => ${targetHome}\n\tpid = ${hungPid}\n`, stderr: "" };
          }
          if (args[0] === "kickstart") {
            liveRevisionAtKickstart = buildInfo(layout.live)?.revision ?? null;
            assert.equal(hungAlive, false, "hung successor must be terminated before launchctl kickstart");
            bindControlServer(targetHome, (_req, socket) => {
              socket.end(`${JSON.stringify({ outcome: "status", pid: hungPid + 100, phase: "ready", started_at_unix_ms: 30, stopping: null, interface_failures: [] })}\n`);
            }).then((srv) => { recoveredServer = srv; });
            return { status: 0, stdout: "", stderr: "" };
          }
          return { status: 1, stdout: "", stderr: "unexpected launchctl call" };
        },
      });
      assert.equal(result.ok, false);
      assert.equal(result.rolled_back, true);
      assert.equal(result.recovered, true);
      assert.equal(result.runtime.started_at_unix_ms, 30);
      assert.equal(result.runtime.phase, "ready");
      assert.equal(buildInfo(layout.live).revision, "live");
      assert.equal(buildInfo(layout.failed).revision, "staging");
      assert.equal(liveRevisionAtSignal, "live", "previous bundle must be restored before terminating hung successor");
      assert.equal(liveRevisionAtKickstart, "live", "previous bundle must be restored before launchctl kickstart");
      assert.deepEqual(result.recovery.terminated_pids, [hungPid]);
      assert.deepEqual(signals, ignoreTerm ? [[hungPid, "SIGTERM"], [hungPid, "SIGKILL"]] : [[hungPid, "SIGTERM"]]);
      assert.equal(result.recovery.launchd_loaded, true);
      assert.equal(result.recovery.launchd_kickstarted, true);
      assert.deepEqual(launchctlCalls, [["print", "gui/501/app.octopus.runtime"], ["kickstart", "-k", "gui/501/app.octopus.runtime"]]);
    } finally {
      await initialServer.close();
      if (recoveredServer) await recoveredServer.close();
    }
  }

  // Scenario D: launchd service NOT loaded -> hung PID is still terminated cleanly, no kickstart attempted, recovered: false.
  {
    layout = fakeLayout();
    const targetHome = tempRoot();
    let hungAlive = true;
    const signals = [];
    const launchctlCalls = [];
    const initialServer = await bindControlServer(targetHome, (request, socket) => {
      socket.end(`${JSON.stringify({ outcome: "status", pid: 6300, phase: "ready", started_at_unix_ms: 10, stopping: null, interface_failures: [] })}\n`);
      if (request === "restart") {
        initialServer.close();
        fs.rmSync(path.join(targetHome, "state", "runtime-instance", "control.sock"), { force: true });
      }
    });
    try {
      result = await restartRuntime(layout, targetHome, {
        timeoutMs: 120, statusPollMs: 20, terminateGraceMs: 40, terminatePollMs: 10,
        swap: true, readTurns: idle, platform: "darwin", uid: 501,
        isProcessAlive: (pid) => pid === 6300 && hungAlive,
        sendSignal: (pid, signal) => { signals.push([pid, signal]); hungAlive = false; return { delivered: true }; },
        launchctl: (args) => { launchctlCalls.push(args); return { status: 113, stdout: "", stderr: "Could not find service" }; },
      });
      assert.equal(result.ok, false);
      assert.equal(result.rolled_back, true);
      assert.equal(result.recovered, false);
      assert.deepEqual(signals, [[6300, "SIGTERM"]]);
      assert.deepEqual(result.recovery.terminated_pids, [6300]);
      assert.equal(result.recovery.launchd_loaded, false);
      assert.equal(result.recovery.launchd_kickstarted, false);
      assert.deepEqual(launchctlCalls, [["print", "gui/501/app.octopus.runtime"]]);
      assert.match(result.error, /no runtime answers/);
      assert.equal(buildInfo(layout.live).revision, "live");
    } finally {
      await initialServer.close();
    }
  }

  {
    const prevCodexHome = process.env.CODEX_HOME;
    try {
      const customCodexHome = path.join(tempRoot(), "custom-codex");
      process.env.CODEX_HOME = customCodexHome;
      const customEnv = runtimeEnv("/tmp/Octopus.app", "/tmp/octopus-home");
      assert.equal(customEnv.CODEX_HOME, customCodexHome);
      assert.equal(customEnv.OCTOPUS_MACHINE_CODEX_AUTH_FILE, path.join(customCodexHome, "auth.json"));
      assert.equal(customEnv.OCTOPUS_MACHINE_CODEX_AUTH_LOCK_FILE, path.join(customCodexHome, "auth.lock"));
      delete process.env.CODEX_HOME;
      const defaultEnv = runtimeEnv("/tmp/Octopus.app", "/tmp/octopus-home");
      assert.equal(defaultEnv.CODEX_HOME, path.join(os.homedir(), ".codex"));
      assert.equal(defaultEnv.OCTOPUS_MACHINE_CODEX_AUTH_FILE, path.join(os.homedir(), ".codex", "auth.json"));
      assert.equal(defaultEnv.OCTOPUS_MACHINE_CODEX_AUTH_LOCK_FILE, path.join(os.homedir(), ".codex", "auth.lock"));
    } finally {
      if (prevCodexHome === undefined) delete process.env.CODEX_HOME;
      else process.env.CODEX_HOME = prevCodexHome;
    }
  }

  // Space-separated value options, restart timeout, disk preflight, and safe prune contracts.
  assert.deepEqual(
    parseArgs(["restart", "--home", "/tmp/h", "--install-dir", "/opt/oct", "--service", "octopus.service", "--user-service", "--timeout", "180", "--min-free-gb", "20"]),
    { positional: ["restart"], options: { home: "/tmp/h", installDir: "/opt/oct", service: "octopus.service", userService: true, timeout: "180", minFreeGb: "20" } },
  );
  assert.throws(() => parseArgs(["prune", "--target-dir"]), /requires a value/);
  assert.equal(resolveRestartTimeoutMs({ timeout: "45" }), 45_000);
  assert.throws(() => resolveRestartTimeoutMs({ timeout: "0" }), /between 1 and 3600/);

  assert.equal(resolveMinFreeGb(undefined, undefined), 15);
  assert.equal(resolveMinFreeGb("8", "20"), 8);
  assert.equal(resolveMinFreeGb(undefined, "12.5"), 12.5);
  assert.throws(() => resolveMinFreeGb("0", null), /positive number/);
  const lowStatfs = () => ({ bavail: 1024, bsize: 4096 });
  const highStatfs = () => ({ bavail: 10 * 1024 * 1024, bsize: 4096 });
  assert.equal(checkBuildDiskSpace(tempRoot(), { minFreeGb: 5, statfs: lowStatfs }).ok, false);
  assert.equal(checkBuildDiskSpace(tempRoot(), { minFreeGb: 5, statfs: highStatfs }).ok, true);
  {
    const diskLayout = fakeLayout();
    assert.equal(buildStaging(diskLayout, { dryRun: true, minFreeGb: 5, statfs: lowStatfs }).ok, false);
    assert.throws(() => buildStaging(diskLayout, { minFreeGb: 5, statfs: lowStatfs }), /insufficient free disk space/);
  }
  {
    const ws = tempRoot();
    fs.writeFileSync(path.join(ws, "Cargo.toml"), "[workspace]\n");
    const pruneLayout = bundles(path.join(ws, "app", "build"));
    const pruneHome = path.join(ws, "home");
    fakeBundle(pruneLayout.live, "live");
    fakeBundle(pruneLayout.staging, "staging");
    fakeBundle(pruneLayout.prev, "prev");
    fs.mkdirSync(path.join(ws, "target"), { recursive: true });
    fs.writeFileSync(path.join(ws, "target", "a.bin"), Buffer.alloc(2048));
    fs.mkdirSync(pruneHome, { recursive: true });
    const dry = pruneBuildArtifacts(pruneLayout, pruneHome, { dryRun: true, sourceRoot: ws });
    assert.equal(dry.applied, false);
    assert.ok(dry.candidates.some((c) => c.label === "rust_target"));
    assert.ok(dry.candidates.some((c) => c.label === "staging_bundle"));
    assert.equal(dry.candidates.some((c) => c.label === "previous_bundle"), false);
    const applied = pruneBuildArtifacts(pruneLayout, pruneHome, { sourceRoot: ws });
    assert.equal(applied.applied, true);
    assert.equal(fs.existsSync(pruneLayout.live), true);
    assert.equal(fs.existsSync(pruneLayout.prev), true);
    assert.equal(fs.existsSync(pruneLayout.staging), false);
    assert.equal(fs.existsSync(path.join(ws, "target")), false);
    assert.throws(() => pruneBuildArtifacts(pruneLayout, pruneHome, { targetDir: pruneLayout.live, sourceRoot: ws }), /overlaps protected live_bundle/);
    assert.throws(() => pruneBuildArtifacts(pruneLayout, pruneHome, { targetDir: pruneHome, sourceRoot: ws }), /overlaps protected octopus_home/);
    const otherDir = path.join(ws, "other-dir");
    fs.mkdirSync(otherDir, { recursive: true });
    const symTarget = path.join(ws, "sym-target");
    fs.symlinkSync(otherDir, symTarget);
    assert.throws(() => pruneBuildArtifacts(pruneLayout, pruneHome, { targetDir: symTarget, sourceRoot: ws }), /symlinked rust_target/);
  }

  // Linux headless CLI + systemd parity checks (cross-platform with mocked systemctl/spawn).
  function fakeCliBundle(bundleDir, version, { mode = "checkout", platform = "linux", includeAuthority = true, badSha = false } = {}) {
    const cliDir = mode === "standalone" ? path.join(bundleDir, "app", "build", "cli") : bundleDir;
    const runtimeRoot = path.join(cliDir, "runtime");
    const binDir = path.join(runtimeRoot, "bin");
    fs.mkdirSync(binDir, { recursive: true });
    fs.writeFileSync(path.join(cliDir, "shared.cjs"), "module.exports = {};\n");
    const octBin = path.join(binDir, "octopus");
    fs.writeFileSync(octBin, "#!/bin/sh\necho 'usage: octopus <up|doctor>' >&2\nexit 2\n", { mode: 0o755 });
    const bins = [{ path: "bin/octopus", file: octBin }];
    if (includeAuthority) {
      const authBin = path.join(binDir, "octopus-linux-process-authority");
      fs.writeFileSync(authBin, "#!/bin/sh\nexit 0\n", { mode: 0o755 });
      bins.push({ path: "bin/octopus-linux-process-authority", file: authBin });
    }
    const receiptBins = bins.map(({ path: rel, file }) => {
      const bytes = fs.readFileSync(file);
      return { path: rel, bytes: bytes.length, sha256: badSha ? "0".repeat(64) : crypto.createHash("sha256").update(bytes).digest("hex") };
    });
    fs.writeFileSync(path.join(runtimeRoot, "runtime-build-receipt.json"), JSON.stringify({
      schema: "octopus.cli_runtime_distribution", schema_version: 1, package_version: version,
      platform, arch: "x64", binaries: receiptBins,
    }));
    if (mode === "standalone") {
      fs.mkdirSync(path.join(bundleDir, "app", "cli"), { recursive: true });
      fs.writeFileSync(path.join(bundleDir, "app", "cli", "index.cjs"), "console.log('octopus cli');\n");
      fs.writeFileSync(path.join(bundleDir, "octopus"), "#!/bin/sh\necho 'Octopus CLI'\nexit 0\n", { mode: 0o755 });
    }
    for (const dir of [bundleDir, path.join(bundleDir, "app"), path.join(bundleDir, "app", "build"), cliDir, runtimeRoot, binDir]) {
      if (fs.existsSync(dir)) fs.chmodSync(dir, 0o755);
    }
  }

  {
    const cliRoot = tempRoot();
    const installDir = path.join(cliRoot, "opt-octopus");
    const cliLayout = bundles(path.join(cliRoot, "build"), { platform: "linux", installDir });
    assert.equal(cliLayout.kind, "linux-cli");
    assert.equal(cliLayout.live, installDir);
    assert.equal(cliLayout.staging, `${installDir}.staging`);
    assert.equal(cliLayout.prev, `${installDir}.prev`);

    const parsedExec = parseSystemctlExecStart("{ path=/opt/octopus/octopus ; argv[]=/opt/octopus/octopus up --foreground --bundle /opt/octopus ; }");
    assert.equal(parsedExec.path, "/opt/octopus/octopus");
    assert.equal(parsedExec.bundleArg, "/opt/octopus");
    assert.equal(parseSystemctlEnvironment("OCTOPUS_HOME=/var/lib/oct PATH=/usr/bin").OCTOPUS_HOME, "/var/lib/oct");
    const parsedShow = parseSystemctlShow("LoadState=loaded\nActiveState=active\nSubState=running\nMainPID=4242\nNRestarts=0\nExecStart={ path=/opt/octopus/octopus ; argv[]=/opt/octopus/octopus up ; }\nEnvironment=OCTOPUS_HOME=/var/lib/oct\n");
    assert.equal(parsedShow.mainPid, 4242);
    assert.equal(parsedShow.octopusHome, "/var/lib/oct");

    assert.equal(isValidRuntimeUsageOutput({ status: 2, stderr: "Usage: octopus <up|doctor>" }), true);
    assert.equal(isValidRuntimeUsageOutput({ status: 2, stderr: "unrecognized error" }), false);

    // Standalone verifyStaging + layout parity + fail-closed checks + standalone prune skipping default rust_target.
    const cliHome = path.join(cliRoot, "home");
    fs.mkdirSync(cliHome, { recursive: true });
    fakeCliBundle(cliLayout.live, "0.1.0", { mode: "standalone" });
    fakeCliBundle(cliLayout.staging, "0.1.1", { mode: "standalone" });
    const mockSystemctl = (activeAuth = "active", homeOverride = cliHome) => (args) => {
      if (args[0] === "show" && args[1].includes("authority")) {
        return { status: 0, stdout: `LoadState=loaded\nActiveState=${activeAuth}\nSubState=running\nMainPID=500\n`, stderr: "" };
      }
      if (args[0] === "show") {
        return { status: 0, stdout: `LoadState=loaded\nActiveState=active\nSubState=running\nMainPID=600\nNRestarts=0\nExecStart={ path=${cliLayout.live}/octopus ; argv[]=${cliLayout.live}/octopus up ; }\nEnvironment=OCTOPUS_HOME=${homeOverride}\n`, stderr: "" };
      }
      return { status: 0, stdout: "", stderr: "" };
    };
    const verified = await verifyStaging(cliLayout, cliHome, { platform: "linux", systemctl: mockSystemctl() });
    assert.equal(verified.ok, true, JSON.stringify(verified.checks));

    // Layout parity fails closed when checkout staging is paired against standalone live install.
    fs.rmSync(cliLayout.staging, { recursive: true, force: true });
    fakeCliBundle(cliLayout.staging, "0.1.1", { mode: "checkout" });
    assert.equal(checkCliLayoutParity(cliLayout, inspectSystemdService(cliLayout, cliHome, { platform: "linux", systemctl: mockSystemctl() })).ok, false);
    // Corrupt sha256 fails closed.
    fs.rmSync(cliLayout.staging, { recursive: true, force: true });
    fakeCliBundle(cliLayout.staging, "0.1.1", { mode: "standalone", badSha: true });
    const corrupt = await verifyStaging(cliLayout, cliHome, { platform: "linux", systemctl: mockSystemctl() });
    assert.equal(corrupt.ok, false);
    assert.equal(corrupt.checks.find((c) => c.name === "receipt_binaries").ok, false);

    // Standalone bundle prune skips default rust_target (no Cargo.toml & overlaps live_bundle), while explicit --target-dir fails closed.
    fs.mkdirSync(path.join(cliLayout.live, "target"), { recursive: true });
    fs.writeFileSync(path.join(cliLayout.live, "target", "keep.txt"), "keep");
    const standalonePrune = pruneBuildArtifacts(cliLayout, cliHome, { dryRun: true, includeCli: true, sourceRoot: cliLayout.live });
    assert.equal(standalonePrune.ok, true);
    assert.ok(standalonePrune.skipped.some((s) => s.label === "cli_build" && s.reason === "protected_live_bundle"));
    assert.ok(standalonePrune.skipped.some((s) => s.label === "rust_target" && s.reason === "protected_live_bundle"));
    assert.throws(() => pruneBuildArtifacts(cliLayout, cliHome, { targetDir: path.join(cliLayout.live, "target"), sourceRoot: cliLayout.live }), /overlaps protected live_bundle/);
    const cliStatus = await status(cliLayout, cliHome, { platform: "linux", systemctl: mockSystemctl() });
    assert.equal(cliStatus.kind, "linux-cli");
    assert.equal(cliStatus.systemd.matchesTarget, true);
  }

  // Linux systemd restart --swap: mismatch refusal, authority refusal, permission-denied rollback, happy swap, and crash-loop rollback.
  {
    const base = tempRoot();
    const installDir = path.join(base, "octopus");
    const cliLayout = bundles(path.join(base, "build"), { platform: "linux", installDir });
    const resetBundles = () => {
      for (const dir of [cliLayout.live, cliLayout.staging, cliLayout.prev, cliLayout.failed]) fs.rmSync(dir, { recursive: true, force: true });
      fakeCliBundle(cliLayout.live, "live-v1", { mode: "standalone" });
      fakeCliBundle(cliLayout.staging, "stage-v2", { mode: "standalone" });
    };
    resetBundles();
    const rt = await fakeRuntime([]);
    try {
      // Mismatch refusal & requireAuthority refusal
      const mismatch = await restartRuntime(cliLayout, rt.home, {
        ...options, swap: true, readTurns: idle, platform: "linux",
        systemctl: () => ({ status: 0, stdout: `LoadState=loaded\nActiveState=active\nSubState=running\nMainPID=42\nExecStart={ path=/other/octopus ; }\nEnvironment=OCTOPUS_HOME=${rt.home}\n`, stderr: "" }),
      });
      assert.equal(mismatch.refused, "systemd_service_mismatch");
      const noAuth = await restartRuntime(cliLayout, rt.home, {
        ...options, swap: true, readTurns: idle, platform: "linux", requireAuthority: true,
        systemctl: (args) => args[1].includes("authority")
          ? { status: 0, stdout: "LoadState=loaded\nActiveState=inactive\nSubState=dead\nMainPID=0\n", stderr: "" }
          : { status: 0, stdout: `LoadState=loaded\nActiveState=active\nSubState=running\nMainPID=42\nExecStart={ path=${cliLayout.live}/octopus ; }\nEnvironment=OCTOPUS_HOME=${rt.home}\n`, stderr: "" },
      });
      assert.equal(noAuth.refused, "process_authority_inactive");

      // Permission denied on systemctl restart -> rolls back swapped bundle without sudo.
      const permDenied = await restartRuntime(cliLayout, rt.home, {
        ...options, swap: true, readTurns: idle, platform: "linux", userService: false,
        systemctl: (args) => {
          if (args[0] === "restart") return { status: 1, stdout: "", stderr: "Interactive authentication required." };
          return { status: 0, stdout: `LoadState=loaded\nActiveState=active\nSubState=running\nMainPID=42\nExecStart={ path=${cliLayout.live}/octopus ; }\nEnvironment=OCTOPUS_HOME=${rt.home}\n`, stderr: "" };
        },
      });
      assert.equal(permDenied.ok, false);
      assert.equal(permDenied.rolled_back, true);
      assert.match(permDenied.error, /never invokes sudo/);
      assert.equal(buildInfo(cliLayout.live, cliLayout).package_version, "live-v1");

      // Happy path swap + systemctl --user restart.
      resetBundles();
      const restartCalls = [];
      const happy = await restartRuntime(cliLayout, rt.home, {
        ...options, swap: true, readTurns: idle, platform: "linux", userService: true, statusPollMs: 15,
        systemctl: (args, opts) => {
          if (args[0] === "restart") {
            restartCalls.push({ args, userService: opts?.userService });
            rt.state.started += 1;
            return { status: 0, stdout: "", stderr: "" };
          }
          return { status: 0, stdout: `LoadState=loaded\nActiveState=active\nSubState=running\nMainPID=42\nNRestarts=0\nExecStart={ path=${cliLayout.live}/octopus ; }\nEnvironment=OCTOPUS_HOME=${rt.home}\n`, stderr: "" };
        },
      });
      assert.equal(happy.ok, true);
      assert.equal(restartCalls.length, 1);
      assert.equal(restartCalls[0].userService, true);
      assert.equal(buildInfo(cliLayout.live, cliLayout).package_version, "stage-v2");
      assert.equal(buildInfo(cliLayout.prev, cliLayout).package_version, "live-v1");

      // Crash-loop short-circuit rollback (succeeds far before 60s timeout).
      resetBundles();
      let restartCount = 0;
      const startedAt = Date.now();
      const crashLoop = await restartRuntime(cliLayout, rt.home, {
        timeoutMs: 60_000, statusPollMs: 15, swap: true, readTurns: idle, platform: "linux", userService: true,
        systemctl: (args) => {
          if (args[0] === "restart") {
            restartCount += 1;
            if (restartCount === 2) rt.state.started += 1;
            return { status: 0, stdout: "", stderr: "" };
          }
          const sub = restartCount === 1 ? "auto-restart" : "running";
          const active = restartCount === 1 ? "activating" : "active";
          return { status: 0, stdout: `LoadState=loaded\nActiveState=${active}\nSubState=${sub}\nMainPID=42\nNRestarts=${restartCount === 1 ? 2 : 0}\nExecStart={ path=${cliLayout.live}/octopus ; }\nEnvironment=OCTOPUS_HOME=${rt.home}\n`, stderr: "" };
        },
      });
      assert.equal(crashLoop.ok, false);
      assert.equal(crashLoop.rolled_back, true);
      assert.equal(crashLoop.recovered, true);
      assert.equal(restartCount, 2);
      assert.ok(Date.now() - startedAt < 5000, "crash-loop must abort readiness wait immediately");
      assert.equal(buildInfo(cliLayout.live, cliLayout).package_version, "live-v1");
      assert.equal(buildInfo(cliLayout.failed, cliLayout).package_version, "stage-v2");
    } finally {
      await rt.close();
    }
  }

  console.log("safe self-update checks passed");
}

main().finally(() => { for (const root of roots) fs.rmSync(root, { recursive: true, force: true }); })
  .catch((error) => { console.error(error); process.exitCode = 1; });
