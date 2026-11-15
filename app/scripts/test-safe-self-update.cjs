const assert = require("node:assert/strict");
const fs = require("node:fs");
const net = require("node:net");
const os = require("node:os");
const path = require("node:path");
const { bundles, buildInfo, swapBundles, rollbackBundles, control, blockingTurns, packageAction, restartRuntime, parseArgs } = require("./safe-self-update.cjs");

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

function fakeRuntime(successorPhases) {
  const home = tempRoot(), directory = path.join(home, "state", "runtime-instance");
  fs.mkdirSync(directory, { recursive: true });
  const state = { started: 1, phase: "ready", requests: [] };
  const server = net.createServer((socket) => {
    socket.once("data", (chunk) => {
      const { request } = JSON.parse(String(chunk));
      state.requests.push(request);
      socket.end(`${JSON.stringify({ outcome: "status", pid: 42, phase: state.phase, started_at_unix_ms: state.started, stopping: null, interface_failures: [] })}\n`);
      if (request === "restart") { state.phase = successorPhases[state.started - 1] ?? "ready"; state.started += 1; }
    });
  });
  return new Promise((resolve) => server.listen(path.join(directory, "control.sock"), () => resolve({ home, state, close: () => new Promise((done) => server.close(done)) })));
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

  assert.equal(packageAction({ id: "x", update_available: false }), "current");
  assert.equal(packageAction({ id: "x", update_available: true, local_changes: [] }), "repair");
  assert.equal(packageAction({ id: "x", update_available: true, local_changes: [{}] }), "review_local_changes");
  assert.equal(packageAction({ id: "world_operator_chat", update_available: true }), "repair_with_client_restart");
  assert.equal(packageAction({ id: "x", update_available: true, error: "stale" }), "blocked");

  assert.deepEqual(parseArgs(["restart", "--swap", "--caller-run=run_1"]), { positional: ["restart"], options: { swap: true, callerRun: "run_1" } });
  assert.equal((await control(tempRoot(), "status", 500)).outcome, "unreachable");

  layout = fakeLayout();
  let runtime = await fakeRuntime([]);
  let result = await restartRuntime(layout, runtime.home, { ...options, swap: true, callerRun: "me",
    readTurns: async () => [{ run_id: "me", turn_open: true }, { run_id: "other", turn_open: true }] });
  assert.equal(result.refused, "turns_in_flight");
  assert.deepEqual(result.blocking.map((chat) => chat.run_id), ["other"]);
  assert.deepEqual(runtime.state.requests, ["status"]);
  assert.equal(buildInfo(layout.live).revision, "live");

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

  layout = fakeLayout();
  await assert.rejects(restartRuntime(layout, tempRoot(), { ...options, swap: true, readTurns: idle }), /no runtime answers/);
  assert.equal(buildInfo(layout.live).revision, "live");
  console.log("safe self-update checks passed");
}

main().finally(() => { for (const root of roots) fs.rmSync(root, { recursive: true, force: true }); })
  .catch((error) => { console.error(error); process.exitCode = 1; });
