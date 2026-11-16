const assert = require("node:assert/strict");
const crypto = require("node:crypto");
const fs = require("node:fs");
const net = require("node:net");
const os = require("node:os");
const path = require("node:path");
const { bundles, buildInfo, swapBundles, rollbackBundles, control, readLedgerTurns, readTurnsInFlight, blockingTurns, newInterfaceFailures,
  packageAction, restartRuntime, parseArgs, main: selfUpdate, readStrictJson, canonicalJson, loadReceipt, receiptGate, verifyStaging } = require("./safe-self-update.cjs");

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
  const state = { started: 1, phase: "ready", requests: [] };
  const server = net.createServer((socket) => {
    socket.once("data", (chunk) => {
      const { request } = JSON.parse(String(chunk));
      state.requests.push(request);
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
  let seq = 0;
  const blob = (value) => {
    const name = crypto.createHash("sha256").update(`${seq}:${JSON.stringify(value)}`).digest("hex");
    fs.writeFileSync(path.join(blobs, name), JSON.stringify(value));
    return name;
  };
  const row = (run, label, content, at, kind = "InterfaceArrival") => {
    fs.appendFileSync(path.join(directory, "movements.jsonl"), `${JSON.stringify({ seq, kind, recorded_at_unix_ms: at,
      from: { Interface: "provider" }, to: { Interface: "worker" }, artifact: { id: `${label}:${"0".repeat(64)}`, content },
      invocation_metadata: { runtime: { run } } })}\n`);
    return seq++;
  };
  return {
    dispatch: (run, method, at = 100, options = {}) => {
      const payload = { label: "provider.transport.dispatch", method };
      const name = options.manifest ? blob({ label: "provider.transport.dispatch", payload_content_hash: blob(payload) }) : blob(payload);
      return row(run, "provider.transport.dispatch", [...Buffer.from(name, "hex")], at, options.kind);
    },
    unreadableDispatch: (run, at = 100) => row(run, "provider.transport.dispatch", Array(32).fill(7), at),
    close: (run, label, at = 100) => row(run, label, Array(32).fill(1), at),
    raw: (text) => fs.appendFileSync(path.join(directory, "movements.jsonl"), `${text}\n`),
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

// A receipt and its SHA-256 as the bench producer wrote them: Python's
// json.dumps with sorted keys, compact separators and ASCII-only output.
const PRODUCER_RECEIPT = String.raw`{"schema":"octopus.linux_bench_receipt.v1","commit":"1111111111111111111111111111111111111111","commit_subject":"Na\u00efve caf\u00e9 \u2014 \u0442\u0435\u0441\u0442 \ud83d\udc19 \"quoted\" back\\slash\ttab","os_arch":"linux-x86_64","metrics":{"cargo_check":{"rc":0,"wall_sec":49.419},"rust_workspace":{"pass_rate_pct":100.0,"tiny":1e-05,"failed_tests":[]},"node_contracts":{"failed_suites":[]},"python_sensors_verifiers":{"failed_files":[]}},"zeta":{"b":1,"a":[true,false,null],"\u00e9":"x","Z":-0.0},"digests":{"canonical_serialization":"json-sorted-keys-compact-utf8","sha256":"725971f2b6db50af3654efddb4bffd10a4ba9e9f0d10fcabd3dcabf0ef95e303"}}`;
const PRODUCER_SHA256 = "725971f2b6db50af3654efddb4bffd10a4ba9e9f0d10fcabd3dcabf0ef95e303";

function receiptFile(text) {
  const file = path.join(tempRoot(), "receipt.json");
  fs.writeFileSync(file, text);
  return file;
}

function receipt(commit, edit = () => {}) {
  const payload = { schema: "octopus.linux_bench_receipt.v1", commit, os_arch: "linux-x86_64", timestamp_utc: "2026-01-01T00:00:00Z",
    metrics: { cargo_check: { rc: 0, wall_sec: 1.5 }, rust_workspace: { failed_tests: [] }, node_contracts: { failed_suites: [] },
      python_sensors_verifiers: { failed_files: [] } } };
  edit(payload);
  const sha256 = crypto.createHash("sha256").update(canonicalJson(readStrictJson(JSON.stringify(payload))), "utf8").digest("hex");
  return receiptFile(JSON.stringify({ ...payload, digests: { canonical_serialization: "json-sorted-keys-compact-utf8", sha256 } }, null, 2));
}

async function receiptGateChecks() {
  const candidateCommit = "c".repeat(40), referenceCommit = "a".repeat(40);
  const staged = () => fakeLayout({ live: referenceCommit, staging: candidateCommit });

  // The canonical form is the producer's, not JSON.stringify's: 100.0, 1e-05 and -0.0 keep their spelling,
  // non-ASCII text is escaped per UTF-16 unit, and keys sort by code point.
  assert.equal(loadReceipt(receiptFile(PRODUCER_RECEIPT)).sha256, PRODUCER_SHA256);
  const unescaped = PRODUCER_RECEIPT.replace(/\\u([0-9a-f]{4})/g, (_, unit) => String.fromCharCode(parseInt(unit, 16)));
  assert.notEqual(unescaped, PRODUCER_RECEIPT);
  assert.equal(loadReceipt(receiptFile(unescaped)).sha256, PRODUCER_SHA256);
  assert.equal(loadReceipt(receiptFile(JSON.stringify(JSON.parse(PRODUCER_RECEIPT), null, 2).replace("100,", "100.0,").replace("0.00001", "1e-05")
    .replace('"Z": 0', '"Z": -0.0'))).sha256, PRODUCER_SHA256);
  assert.throws(() => loadReceipt(receiptFile(JSON.stringify(JSON.parse(PRODUCER_RECEIPT)))), /sha256 mismatch/);
  assert.throws(() => readStrictJson('{"a":1,"a":2}'), /duplicate key/);
  assert.throws(() => readStrictJson('{"a":1} x'), /trailing content/);
  assert.throws(() => readStrictJson('{"a":01}'), /not strict JSON/);
  assert.throws(() => loadReceipt(path.join(tempRoot(), "absent.json")), /no such file/);
  assert.throws(() => loadReceipt(receipt(candidateCommit, (payload) => { payload.schema = "another.schema"; })), /schema is/);

  let layout = staged();
  const good = receipt(candidateCommit);
  assert.equal(receiptGate(layout.staging), null);
  assert.throws(() => receiptGate(layout.staging, { referenceReceipt: good }), /requires --require-receipt/);
  let gate = receiptGate(layout.staging, { requireReceipt: good });
  assert.equal(gate.ok, true);
  assert.equal(gate.compared, false);
  assert.equal(gate.candidate.commit, candidateCommit);

  const tampered = receiptFile(fs.readFileSync(good, "utf8").replace('"rc": 0', '"rc": 0, "extra": 1'));
  gate = receiptGate(layout.staging, { requireReceipt: tampered });
  assert.equal(gate.ok, false);
  assert.match(gate.detail, /sha256 mismatch/);
  assert.equal(gate.candidate, null);

  gate = receiptGate(layout.staging, { requireReceipt: receipt(referenceCommit) });
  assert.equal(gate.ok, false);
  assert.match(gate.detail, /candidate receipt is for commit/);
  gate = receiptGate(layout.staging, { requireReceipt: receipt(candidateCommit, (payload) => { payload.metrics.cargo_check.rc = 101; }) });
  assert.equal(gate.ok, false);
  assert.match(gate.detail, /cargo check rc 101/);
  gate = receiptGate(layout.staging, { requireReceipt: receipt(candidateCommit, (payload) => { delete payload.metrics.cargo_check; }) });
  assert.match(gate.detail, /cargo check rc null/);

  const failing = (tests, suites = [], files = []) => (payload) => {
    payload.metrics.rust_workspace.failed_tests = tests;
    payload.metrics.node_contracts.failed_suites = suites;
    payload.metrics.python_sensors_verifiers.failed_files = files;
  };
  const reference = receipt(referenceCommit, failing(["suite::a", "suite::b"], ["node-x"], ["sensor_y.py"]));
  gate = receiptGate(layout.staging, { requireReceipt: receipt(candidateCommit, failing(["suite::b"])), referenceReceipt: reference });
  assert.equal(gate.ok, true);
  assert.equal(gate.compared, true);
  assert.deepEqual(gate.new_failures, { rust_workspace: [], node_contracts: [], python_sensors_verifiers: [] });
  gate = receiptGate(layout.staging, { requireReceipt: receipt(candidateCommit, failing(["suite::a", "suite::b"], ["node-x"], ["sensor_y.py"])),
    referenceReceipt: reference });
  assert.equal(gate.ok, true);

  // The same number of failures as the reference, but one of them is another test.
  const swapped = receipt(candidateCommit, failing(["suite::a", "suite::c"], ["node-x"], ["sensor_y.py"]));
  gate = receiptGate(layout.staging, { requireReceipt: swapped, referenceReceipt: reference });
  assert.equal(gate.ok, false);
  assert.deepEqual(gate.new_failures, { rust_workspace: ["suite::c"], node_contracts: [], python_sensors_verifiers: [] });
  assert.match(gate.detail, /rust_workspace has 1 failure\(s\) the reference does not have: suite::c/);
  gate = receiptGate(layout.staging, { requireReceipt: receipt(candidateCommit, failing([], ["node-z"], ["sensor_w.py"])), referenceReceipt: reference });
  assert.deepEqual(gate.new_failures, { rust_workspace: [], node_contracts: ["node-z"], python_sensors_verifiers: ["sensor_w.py"] });

  // A failure list that is absent is not an empty one, and a damaged reference proves nothing.
  gate = receiptGate(layout.staging, { requireReceipt: good, referenceReceipt: receipt(referenceCommit, (payload) => { delete payload.metrics.node_contracts; }) });
  assert.equal(gate.ok, false);
  assert.match(gate.detail, /reference receipt has no list of names at metrics.node_contracts.failed_suites/);
  gate = receiptGate(layout.staging, { requireReceipt: receipt(candidateCommit, (payload) => { payload.metrics.rust_workspace.failed_tests = 4; }), referenceReceipt: reference });
  assert.match(gate.detail, /candidate receipt has no list of names at metrics.rust_workspace.failed_tests/);
  gate = receiptGate(layout.staging, { requireReceipt: good, referenceReceipt: tampered });
  assert.equal(gate.ok, false);
  assert.match(gate.detail, /reference receipt .* sha256 mismatch/);
  gate = receiptGate(layout.staging, { requireReceipt: good, referenceReceipt: receipt(referenceCommit, (payload) => { payload.os_arch = "linux-aarch64"; }) });
  assert.match(gate.detail, /measured on "linux-x86_64", the reference on "linux-aarch64"/);

  // verify records the gate as a check of its own.
  let verified = await verifyStaging(layout, tempRoot(), { smoke: false, requireReceipt: good, referenceReceipt: reference });
  assert.deepEqual(verified.checks.find((row) => row.name === "receipt_gate").ok, true);
  assert.equal(verified.receipt_gate.candidate.commit, candidateCommit);
  verified = await verifyStaging(layout, tempRoot(), { smoke: false, requireReceipt: swapped, referenceReceipt: reference });
  assert.equal(verified.ok, false);
  assert.equal(verified.checks.find((row) => row.name === "receipt_gate").ok, false);
  assert.deepEqual(verified.receipt_gate.new_failures.rust_workspace, ["suite::c"]);
  verified = await verifyStaging(layout, tempRoot(), { smoke: false });
  assert.equal(verified.checks.some((row) => row.name === "receipt_gate"), false);

  // swap refuses before it touches a bundle, and applies once the gate passes.
  const buildRoot = `--build-root=${layout.buildRoot}`;
  let result = await selfUpdate(["swap", buildRoot, `--require-receipt=${swapped}`, "--reference-receipt", reference]);
  assert.deepEqual([result.ok, result.applied, result.refused], [false, false, "receipt_gate"]);
  assert.deepEqual(result.new_failures.rust_workspace, ["suite::c"]);
  assert.equal(buildInfo(layout.live).revision, referenceCommit);
  assert.equal(buildInfo(layout.staging).revision, candidateCommit);
  assert.equal(fs.existsSync(layout.prev), false);
  result = await selfUpdate(["swap", buildRoot, "--require-receipt", tampered]);
  assert.equal(result.refused, "receipt_gate");
  assert.equal(buildInfo(layout.live).revision, referenceCommit);
  await assert.rejects(selfUpdate(["swap", buildRoot, `--reference-receipt=${reference}`]), (error) => error.usage === true && /requires --require-receipt/.test(error.message));
  await assert.rejects(selfUpdate(["rollback", buildRoot, `--require-receipt=${good}`]), (error) => error.usage === true);
  assert.throws(() => parseArgs(["swap", "--require-receipt"]), /requires a file/);
  assert.throws(() => parseArgs(["swap", "--require-receipt=a", "--require-receipt=b"]), /more than once/);
  assert.deepEqual(parseArgs(["verify", "--require-receipt", "a.json", "--reference-receipt=b.json"]),
    { positional: ["verify"], options: { requireReceipt: "a.json", referenceReceipt: "b.json" } });
  assert.equal(buildInfo(layout.live).revision, referenceCommit);
  result = await selfUpdate(["swap", buildRoot, `--require-receipt=${good}`, `--reference-receipt=${reference}`]);
  assert.equal(result.applied, true);
  assert.equal(result.receipt_gate.ok, true);
  assert.equal(buildInfo(layout.live).revision, candidateCommit);
  assert.equal(buildInfo(layout.prev).revision, referenceCommit);

  // restart refuses before the swap and before it asks the runtime to restart.
  layout = staged();
  const runtime = await fakeRuntime([]);
  try {
    result = await restartRuntime(layout, runtime.home, { ...options, swap: true, readTurns: idle, requireReceipt: swapped, referenceReceipt: reference });
    assert.deepEqual([result.ok, result.applied, result.refused], [false, false, "receipt_gate"]);
    assert.deepEqual(result.new_failures.rust_workspace, ["suite::c"]);
    assert.deepEqual(runtime.state.requests, ["status"]);
    assert.equal(buildInfo(layout.live).revision, referenceCommit);
    assert.equal(buildInfo(layout.staging).revision, candidateCommit);
    // Without a swap the successor runs the live bundle, so the receipt must be the live bundle's.
    result = await restartRuntime(layout, runtime.home, { ...options, readTurns: idle, requireReceipt: good });
    assert.equal(result.refused, "receipt_gate");
    assert.equal(runtime.state.requests.includes("restart"), false);
    result = await restartRuntime(layout, runtime.home, { ...options, swap: true, readTurns: idle, requireReceipt: good, referenceReceipt: reference });
    assert.equal(result.ok, true);
    assert.equal(result.revision, candidateCommit);
    assert.equal(runtime.state.requests.filter((request) => request === "restart").length, 1);
  } finally { await runtime.close(); }
}
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
  assert.deepEqual(await readLedgerTurns(home), []);
  let ledger = fakeLedger(home);
  const openedAt = ledger.dispatch("service_open", "turn/start", 100, { manifest: true });
  ledger.dispatch("service_done", "turn/start");
  ledger.close("service_done", "turn.completed");
  ledger.dispatch("service_failed", "turn/start");
  ledger.close("service_failed", "turn.error");
  ledger.dispatch("service_interrupted", "turn/start");
  ledger.dispatch("service_interrupted", "turn/interrupt");
  ledger.close("service_interrupted", "turn.interrupted.result");
  ledger.dispatch("service_answered", "turn/start");
  ledger.close("service_answered", "interface.artifact.output");
  ledger.dispatch("only_interrupt", "turn/interrupt");
  ledger.dispatch("outbound_only", "turn/start", 100, { kind: "InterfaceOutbound" });
  ledger.dispatch("before_this_runtime", "turn/start", 10);
  ledger.dispatch("client_chat", "turn/start");
  ledger.raw('{"truncated "provider.transport.dispatch:');
  const unreadableAt = ledger.unreadableDispatch("sensor_unreadable");
  ledger.dispatch("service_reopened", "turn/start");
  ledger.close("service_reopened", "turn.completed");
  const reopenedAt = ledger.dispatch("service_reopened", "turn/start");
  assert.deepEqual(await readLedgerTurns(home, { sinceUnixMs: 50, skipRuns: ["client_chat"] }), [
    { run_id: "service_open", turn_open: true, turn_state: "working", source: "ledger", opened_movement_seq: openedAt },
    { run_id: "sensor_unreadable", turn_open: true, turn_state: "working", source: "ledger", opened_movement_seq: unreadableAt },
    { run_id: "service_reopened", turn_open: true, turn_state: "working", source: "ledger", opened_movement_seq: reopenedAt },
  ]);
  assert.deepEqual((await readLedgerTurns(home)).map((turn) => turn.run_id).sort(),
    ["before_this_runtime", "client_chat", "sensor_unreadable", "service_open", "service_reopened"]);

  layout = fakeLayout();
  fakeProjectionBundle(layout, [{ run_id: "client_chat" }, { run_id: "client_idle" }],
    [{ run_id: "client_chat", turn_open: true, turn_state: "working" }, { run_id: "client_idle", turn_open: false, turn_state: "idle" },
      { run_id: "outside_scope", turn_open: true, turn_state: "working" }]);
  const inFlight = await readTurnsInFlight(layout.live, home, { sinceUnixMs: 50 });
  assert.deepEqual(inFlight.map((turn) => [turn.run_id, turn.source, turn.turn_open]), [
    ["client_chat", "client", true], ["client_idle", "client", false],
    ["service_open", "ledger", true], ["sensor_unreadable", "ledger", true], ["service_reopened", "ledger", true]]);
  assert.deepEqual(blockingTurns(inFlight, "client_chat").map((turn) => turn.run_id), ["service_open", "sensor_unreadable", "service_reopened"]);

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

  // The default reader guards runs without a client chat surface through the ledger.
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

  // One failure before, one failure after, but another interface: a regression, not an equal count.
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

  // A failure that was already there, or one that recovered, does not fail the restart.
  layout = fakeLayout();
  runtime = await fakeRuntime([], [[failure("world_a"), failure("world_b")], [failure("world_a", "still failing")]]);
  result = await restartRuntime(layout, runtime.home, { ...options, swap: true, readTurns: idle });
  assert.equal(result.ok, true);
  assert.equal(result.revision, "staging");
  await runtime.close();

  layout = fakeLayout();
  await assert.rejects(restartRuntime(layout, tempRoot(), { ...options, swap: true, readTurns: idle }), /no runtime answers/);
  assert.equal(buildInfo(layout.live).revision, "live");
  await receiptGateChecks();
  console.log("safe self-update checks passed");
}

main().finally(() => { for (const root of roots) fs.rmSync(root, { recursive: true, force: true }); })
  .catch((error) => { console.error(error); process.exitCode = 1; });
