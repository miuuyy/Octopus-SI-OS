#!/usr/bin/env node
// Safe self-update of an installation that runs from app/build/Octopus.app:
// build -> verify -> swap -> packages sync -> restart. Each step is separate and the
// live bundle is never absent during a build. See docs/development.md.
const fs = require("node:fs");
const net = require("node:net");
const os = require("node:os");
const path = require("node:path");
const { spawn, spawnSync } = require("node:child_process");
const { setTimeout: delay } = require("node:timers/promises");

const appRoot = path.resolve(__dirname, "..");
const repoRoot = path.resolve(appRoot, "..");

function bundles(buildRoot = path.join(appRoot, "build")) {
  const at = (name) => path.join(buildRoot, name);
  return { buildRoot, live: at("Octopus.app"), staging: at("Octopus.staging.app"), prev: at("Octopus.prev.app"), failed: at("Octopus.failed.app") };
}
const installation = (bundle) => path.join(bundle, "Contents", "Resources", "app");
const runtimeBinary = (bundle) => path.join(installation(bundle), "bin", "octopus");

function buildInfo(bundle) {
  try { return JSON.parse(fs.readFileSync(path.join(installation(bundle), "build-info.json"), "utf8")); } catch { return null; }
}

function requireBundle(bundle) {
  if (!fs.statSync(bundle, { throwIfNoEntry: false })?.isDirectory() || !buildInfo(bundle)) {
    throw new Error(`${bundle} is not a packaged Octopus bundle`);
  }
  return bundle;
}

// Octopus.app -> Octopus.prev.app, Octopus.staging.app -> Octopus.app. Two renames on one
// volume; a failed second rename puts the previous bundle back.
function swapBundles(layout, { dryRun = false } = {}) {
  requireBundle(layout.staging);
  const hadLive = fs.existsSync(layout.live);
  const plan = [`remove ${layout.prev}`, ...(hadLive ? [`rename ${layout.live} -> ${layout.prev}`] : []), `rename ${layout.staging} -> ${layout.live}`];
  if (dryRun) return { applied: false, plan };
  if (hadLive) {
    requireBundle(layout.live);
    fs.rmSync(layout.prev, { recursive: true, force: true });
    fs.renameSync(layout.live, layout.prev);
  }
  try { fs.renameSync(layout.staging, layout.live); }
  catch (error) { if (hadLive) fs.renameSync(layout.prev, layout.live); throw error; }
  return { applied: true, plan, revision: buildInfo(layout.live).revision };
}

// Octopus.app -> Octopus.failed.app, Octopus.prev.app -> Octopus.app.
function rollbackBundles(layout, { dryRun = false } = {}) {
  requireBundle(layout.prev);
  const hadLive = fs.existsSync(layout.live);
  const plan = [`remove ${layout.failed}`, ...(hadLive ? [`rename ${layout.live} -> ${layout.failed}`] : []), `rename ${layout.prev} -> ${layout.live}`];
  if (dryRun) return { applied: false, plan };
  fs.rmSync(layout.failed, { recursive: true, force: true });
  if (hadLive) fs.renameSync(layout.live, layout.failed);
  try { fs.renameSync(layout.prev, layout.live); }
  catch (error) { if (hadLive) fs.renameSync(layout.failed, layout.live); throw error; }
  return { applied: true, plan, revision: buildInfo(layout.live).revision };
}

// One request to <home>/state/runtime-instance/control.sock. Never rejects: an absent or
// silent runtime is { outcome: "unreachable" }.
function control(home, request, timeoutMs = 5000) {
  return new Promise((resolve) => {
    const socket = net.createConnection(path.join(home, "state", "runtime-instance", "control.sock"));
    let text = "";
    const done = (value) => { clearTimeout(timer); socket.destroy(); resolve(value); };
    const timer = setTimeout(() => done({ outcome: "unreachable", error: "no answer" }), timeoutMs);
    socket.once("connect", () => socket.write(`${JSON.stringify({ request })}\n`));
    socket.once("error", (error) => done({ outcome: "unreachable", error: error.message }));
    socket.once("end", () => done({ outcome: "unreachable", error: "closed without an answer" }));
    socket.on("data", (chunk) => {
      text += chunk;
      const newline = text.indexOf("\n");
      if (newline < 0) return;
      try { done(JSON.parse(text.slice(0, newline))); } catch (error) { done({ outcome: "unreachable", error: error.message }); }
    });
  });
}

const isReady = (status) => status?.outcome === "status" && status.phase === "ready" && status.stopping === null
  && Array.isArray(status.interface_failures) && status.interface_failures.length === 0;

async function waitForStatus(home, accept, timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    const status = await control(home, "status", 2000);
    if (accept(status)) return { ok: true, status };
    if (Date.now() >= deadline) return { ok: false, status };
    await delay(250);
  }
}

// A minimal, complete environment for a bundle's runtime. /usr/local/bin stays on PATH:
// doctor resolves provider executables such as `claude` there.
function runtimeEnv(bundle, home) {
  const root = installation(bundle), user = os.userInfo().username;
  return {
    HOME: os.homedir(), USER: user, LOGNAME: user, TMPDIR: os.tmpdir(),
    CODEX_HOME: process.env.CODEX_HOME || path.join(os.homedir(), ".codex"),
    PATH: [path.join(root, "bin"), "/usr/local/bin", "/usr/bin", "/bin", "/usr/sbin", "/sbin"].join(":"),
    OCTOPUS_HOME: home, OCTOPUS_INSTALLATION_ROOT: root,
    OCTOPUS_SOURCE_REGISTRY: path.join(root, "registry"), OCTOPUS_RUNTIME_BIN: runtimeBinary(bundle),
  };
}

function octopus(bundle, home, args, options = {}) {
  return spawnSync(runtimeBinary(bundle), args, { env: runtimeEnv(bundle, home), encoding: "utf8", maxBuffer: 64 * 1024 * 1024, ...options });
}

// Turns in flight, read from the client projection of the bundle. The run record's own
// `status` field is not maintained; the projection is. Covers client chat runs only.
async function readChatTurns(bundle, home) {
  const root = installation(bundle);
  const { createProjectionStore } = require(path.join(root, "electron", "projection.cjs"));
  const { projectionReadRequest } = require(path.join(root, "electron", "projectionReadContract.cjs"));
  const store = createProjectionStore({
    resolveRepoRoot: () => repoRoot,
    sourceRegistry: () => path.join(root, "registry"),
    localPackageRegistry: () => path.join(root, "local-packages", "registry"),
    localPackageLibrary: () => path.join(root, "local-packages"),
    octopusHome: () => home,
    projectEePolicies: async () => JSON.parse(octopus(bundle, home, ["project-ee-policies"]).stdout),
  });
  try {
    const scope = await store.read(projectionReadRequest("projection.operator.os.scope"));
    const runIds = (scope.client_runs ?? []).map((row) => row.run_id);
    const status = await store.read(projectionReadRequest("projection.operator.chats.status", { run_ids: runIds }));
    return status.chats.map((chat) => ({ run_id: chat.run_id, turn_open: chat.turn_open === true, turn_state: chat.turn_state }));
  } finally { await store.close?.(); }
}

const blockingTurns = (chats, callerRun) => chats.filter((chat) => chat.turn_open && chat.run_id !== callerRun);

const lastLines = (result) => `${result.stdout ?? ""}${result.stderr ?? ""}`.trim().split("\n").slice(-3).join(" | ");

// Packages into Octopus.staging.app. The packaging log goes to stderr.
function buildStaging(layout, { dryRun = false } = {}) {
  const { node } = JSON.parse(fs.readFileSync(path.join(appRoot, "native", "runtime.json"), "utf8"));
  const env = { ...process.env, OCTOPUS_PACKAGE_BUNDLE: layout.staging,
    OCTOPUS_NATIVE_NODE_HEADERS: process.env.OCTOPUS_NATIVE_NODE_HEADERS || path.join(layout.buildRoot, `node-v${node}`, "include", "node") };
  const command = [process.execPath, path.join(__dirname, "package-macos.cjs")];
  if (dryRun) return { applied: false, command, bundle: layout.staging };
  const identity = () => {
    const stat = fs.statSync(path.join(installation(layout.live), "build-info.json"), { throwIfNoEntry: false });
    return stat ? `${stat.ino}:${stat.mtimeMs}` : null;
  };
  const before = identity();
  const result = spawnSync(command[0], command.slice(1), { cwd: repoRoot, env, stdio: ["ignore", 2, 2] });
  if (result.status !== 0) throw new Error(`packaging failed with status ${result.status}; ${layout.live} was not touched`);
  if (identity() !== before) throw new Error(`${layout.live} changed during the staging build`);
  return { applied: true, bundle: layout.staging, build: buildInfo(layout.staging), live_untouched: true };
}

function packageRoots(bundle, home) {
  const root = installation(bundle), sourceRegistryRoot = path.join(root, "local-packages", "registry");
  const { createPackageStoragePublisher } = require(path.join(root, "electron", "packageStorage.cjs"));
  return { sourceRegistryRoot, activeRegistryRoot: path.join(home, "registry"), libraryRoot: path.join(root, "local-packages"),
    stateRoot: path.join(home, "client", "package-state"), publication: createPackageStoragePublisher(sourceRegistryRoot, root) };
}
const localPackages = (bundle) => require(path.join(installation(bundle), "electron", "localPackages.cjs"));
const CHAT_CLIENT_PACKAGE = "world_operator_chat";

function packageAction(entry) {
  if (entry.available === false || entry.error) return "blocked";
  if (!entry.update_available) return "current";
  if ((entry.local_changes ?? []).length) return "review_local_changes";
  return entry.id === CHAT_CLIENT_PACKAGE ? "repair_with_client_restart" : "repair";
}

// Installed packages of a Home compared with the package library of a bundle. Read-only.
async function packageStatus(bundle, home) {
  const catalog = await localPackages(bundle).inspectLocalPackageCatalog(packageRoots(bundle, home));
  return catalog.packages.filter((entry) => entry.available === false || entry.materialized || entry.partially_materialized || entry.installed).map((entry) => ({
    id: entry.id, action: packageAction(entry), receipt_state: entry.receipt_state ?? "", update_available: entry.update_available === true,
    local_changes: (entry.local_changes ?? []).length, error: entry.error || "",
    expected: { sourceDigest: entry.source_digest, installedDigest: entry.installed_digest, receiptState: entry.receipt_state, receiptDigest: entry.receipt_digest || null },
  }));
}

// Baseline registry files (copied once, at first boot) that differ from the bundle. Read-only.
function baselineDrift(bundle, home) {
  const source = path.join(installation(bundle), "registry"), rows = [];
  const digest = (file) => require("node:crypto").createHash("sha256").update(fs.readFileSync(file)).digest("hex");
  const walk = (directory) => {
    for (const entry of fs.readdirSync(directory, { withFileTypes: true })) {
      const file = path.join(directory, entry.name);
      if (entry.isDirectory()) { walk(file); continue; }
      if (!entry.isFile() || entry.name.startsWith(".")) continue;
      const relative = path.relative(source, file), active = path.join(home, "registry", relative);
      const state = !fs.existsSync(active) ? "missing" : digest(file) === digest(active) ? "current" : "differs";
      if (state !== "current") rows.push({ path: relative, state });
    }
  };
  walk(source);
  return rows;
}

// Repairs every installed package whose source changed and that has no local changes.
// Packages with local changes or errors are reported, never forced. The chat client
// package is repaired only on request, because a running client must be relaunched after.
async function syncPackages(bundle, home, { dryRun = false, withChatClient = false, only = null } = {}) {
  const status = await packageStatus(bundle, home);
  const selected = status.filter((row) => (row.action === "repair" || (withChatClient && row.action === "repair_with_client_restart"))
    && (!only || only.includes(row.id)));
  const skipped = status.filter((row) => !selected.includes(row) && row.action !== "current").map(({ id, action, error }) => ({ id, action, error }));
  if (dryRun) return { applied: false, repair: selected.map((row) => row.id), skipped };
  const results = [];
  for (const [index, row] of selected.entries()) {
    try {
      const outcome = await localPackages(bundle).repairLocalPackages({ ...packageRoots(bundle, home),
        operationId: `self-update-${Date.now()}-${index}`, packageIds: [row.id], expectedRepairs: { [row.id]: row.expected } });
      results.push({ id: row.id, repaired: true, changed: outcome.changed !== false });
    } catch (error) { results.push({ id: row.id, repaired: false, error: error.message }); }
  }
  return { applied: true, results, skipped };
}

// Boots a disposable Home from the bundle: install packages, doctor, up, restart, down.
async function smokeTest(bundle, packageIds) {
  const base = process.platform === "darwin" ? "/private/tmp" : fs.realpathSync(os.tmpdir());
  const home = fs.mkdtempSync(path.join(base, "octo-su-")), rows = [];
  const row = (name, ok, detail = "") => { rows.push({ name: `smoke_${name}`, ok, detail }); return ok; };
  try {
    fs.mkdirSync(path.join(home, "registry"), { recursive: true });
    const roots = packageRoots(bundle, home), library = localPackages(bundle);
    const byId = new Map((await library.inspectLocalPackageCatalog(roots)).packages.map((entry) => [entry.id, entry]));
    const closure = [];
    const visit = (id) => {
      if (closure.includes(id)) return;
      if (!byId.has(id)) throw new Error(`the bundle has no package '${id}'`);
      closure.push(id);
      (byId.get(id).required_packages ?? []).forEach(visit);
    };
    packageIds.forEach(visit);
    await library.installLocalPackages({ ...roots, operationId: `self-update-smoke-${Date.now()}`, packageIds: closure,
      expectedSourceDigests: Object.fromEntries(closure.map((id) => [id, byId.get(id).source_digest])) });
    row("install", true, `${closure.length} packages`);
    const doctor = octopus(bundle, home, ["doctor"]);
    if (!row("doctor", doctor.status === 0, lastLines(doctor))) return rows;
    const log = fs.openSync(path.join(home, "up.log"), "a");
    spawn(runtimeBinary(bundle), ["up"], { env: runtimeEnv(bundle, home), cwd: home, detached: true, stdio: ["ignore", log, log] }).unref();
    fs.closeSync(log);
    const up = await waitForStatus(home, isReady, 60_000);
    if (!row("boot", up.ok, JSON.stringify(up.status))) return rows;
    const restart = octopus(bundle, home, ["restart"]), after = await control(home, "status");
    if (!row("restart", restart.status === 0 && isReady(after) && after.started_at_unix_ms !== up.status.started_at_unix_ms, lastLines(restart))) return rows;
    const down = octopus(bundle, home, ["down"]);
    const stopped = await waitForStatus(home, (status) => status.outcome === "unreachable", 30_000);
    row("stop", down.status === 0 && stopped.ok, lastLines(down));
  } catch (error) { row("error", false, error.message); } finally {
    if ((await control(home, "status", 1000)).outcome === "status") {
      await control(home, "stop");
      await waitForStatus(home, (status) => status.outcome === "unreachable", 15_000);
    }
    fs.rmSync(home, { recursive: true, force: true });
  }
  return rows;
}

// Checks a staged bundle before it may replace the live one.
async function verifyStaging(layout, home, { smoke = true, allowModifiedSource = false, smokePackages = [CHAT_CLIENT_PACKAGE, "world_provider_codex"] } = {}) {
  const bundle = requireBundle(layout.staging), checks = [];
  const check = (name, ok, detail = "") => checks.push({ name, ok, detail });
  if (process.platform === "darwin") {
    const signed = spawnSync("codesign", ["--verify", "--deep", "--strict", bundle], { encoding: "utf8" });
    check("codesign", signed.status === 0, lastLines(signed));
  }
  const info = buildInfo(bundle), head = spawnSync("git", ["rev-parse", "HEAD"], { cwd: repoRoot, encoding: "utf8" }).stdout.trim();
  check("revision", info.revision === head && (allowModifiedSource || info.modifiedSource === false),
    `bundle ${info.revision}${info.modifiedSource ? " built from modified source" : ""}; HEAD ${head}`);
  if (fs.existsSync(path.join(home, "registry"))) {
    const doctor = octopus(bundle, home, ["doctor"]);
    check("doctor_home", doctor.status === 0, lastLines(doctor));
  }
  if (smoke) checks.push(...await smokeTest(bundle, smokePackages));
  return { ok: checks.every((row) => row.ok), bundle, build: info, checks };
}

// Guarded restart of the running runtime through its control socket (what `octopus restart`
// sends). Refuses while another run has a turn in flight. With swap, the staged bundle
// replaces the live one after the guard passes. A successor that is not ready in time is
// rolled back to Octopus.prev.app and restarted again.
async function restartRuntime(layout, home, options = {}) {
  const { callerRun = null, dryRun = false, force = false, swap = false, rollback = true,
    launchdLabel = "app.octopus.runtime", timeoutMs = 300_000, readTurns = readChatTurns } = options;
  const before = await control(home, "status");
  if (before.outcome !== "status") throw new Error(`no runtime answers for Home ${home} (${before.error}); only a running runtime is restarted`);
  if (swap) requireBundle(layout.staging);
  const blocking = blockingTurns(await readTurns(layout.live, home), callerRun);
  const plan = [...(swap ? swapBundles(layout, { dryRun: true }).plan : []), "restart through the control socket", "wait for a ready successor",
    rollback ? `on failure: restore ${layout.prev} and restart again` : "on failure: report only"];
  if (blocking.length && !force) return { ok: false, applied: false, refused: "turns_in_flight", blocking, plan };
  if (dryRun) return { ok: true, applied: false, blocking, plan, runtime: before };
  if (swap) swapBundles(layout);
  const healthy = (status) => status.outcome === "status" && status.phase === "ready" && status.stopping === null
    && status.interface_failures.length <= before.interface_failures.length;
  await control(home, "restart");
  const first = await waitForStatus(home, (status) => healthy(status) && status.started_at_unix_ms !== before.started_at_unix_ms, timeoutMs);
  if (first.ok) return { ok: true, applied: true, swapped: swap, previous: before, runtime: first.status, revision: buildInfo(layout.live)?.revision ?? null };
  if (!rollback || !fs.existsSync(layout.prev)) return { ok: false, applied: true, swapped: swap, rolled_back: false, runtime: first.status };
  rollbackBundles(layout);
  const current = await control(home, "status");
  if (current.outcome === "status") await control(home, "restart");
  else spawnSync("launchctl", ["kickstart", `gui/${process.getuid()}/${launchdLabel}`], { encoding: "utf8" });
  const second = await waitForStatus(home, (status) => healthy(status) && status.started_at_unix_ms !== before.started_at_unix_ms
    && status.started_at_unix_ms !== current.started_at_unix_ms, timeoutMs);
  return { ok: false, applied: true, swapped: swap, rolled_back: true, recovered: second.ok, failed: first.status, runtime: second.status };
}

async function status(layout, home) {
  const describe = (bundle) => (fs.existsSync(bundle) ? buildInfo(bundle) : null);
  const runtime = await control(home, "status");
  let turns = null;
  try { turns = (await readChatTurns(layout.live, home)).filter((chat) => chat.turn_open); } catch (error) { turns = { error: error.message }; }
  return { home, head: spawnSync("git", ["rev-parse", "HEAD"], { cwd: repoRoot, encoding: "utf8" }).stdout.trim(), runtime,
    bundles: { live: describe(layout.live), staging: describe(layout.staging), prev: describe(layout.prev) }, turns_in_flight: turns };
}

const USAGE = `Usage: node app/scripts/safe-self-update.cjs <command> [options]
  status                          live runtime, bundle revisions, turns in flight
  build [--dry-run]               package into app/build/Octopus.staging.app
  verify [--no-smoke] [--allow-modified-source]
  packages status | packages sync [--dry-run] [--with-chat-client] [--only=a,b]
  swap [--dry-run]                Octopus.app -> Octopus.prev.app, staging -> Octopus.app
  restart [--swap] [--caller-run=<run id>] [--dry-run] [--force] [--no-rollback] [--detach --log=<file>]
  rollback [--dry-run]            Octopus.prev.app -> Octopus.app
Common: --home=<Octopus Home, default $OCTOPUS_HOME or ~/.octopus> --build-root=<dir>`;

function parseArgs(argv) {
  const positional = [], options = {};
  for (const arg of argv) {
    if (!arg.startsWith("--")) { positional.push(arg); continue; }
    const [key, ...rest] = arg.slice(2).split("=");
    options[key.replace(/-([a-z])/g, (_, letter) => letter.toUpperCase())] = rest.length ? rest.join("=") : true;
  }
  return { positional, options };
}

async function main(argv) {
  const { positional, options } = parseArgs(argv);
  const layout = bundles(options.buildRoot ? path.resolve(options.buildRoot) : undefined);
  const home = path.resolve(options.home || process.env.OCTOPUS_HOME || path.join(os.homedir(), ".octopus"));
  const dryRun = options.dryRun === true, [command, sub] = positional;
  if (command === "status") return status(layout, home);
  if (command === "build") return buildStaging(layout, { dryRun });
  if (command === "verify") return verifyStaging(layout, home, { smoke: options.noSmoke !== true, allowModifiedSource: options.allowModifiedSource === true });
  if (command === "packages" && sub === "status") return { packages: await packageStatus(layout.live, home), baseline: baselineDrift(layout.live, home) };
  if (command === "packages" && sub === "sync") {
    return syncPackages(layout.live, home, { dryRun, withChatClient: options.withChatClient === true, only: typeof options.only === "string" ? options.only.split(",") : null });
  }
  if (command === "swap") return swapBundles(layout, { dryRun });
  if (command === "rollback") return rollbackBundles(layout, { dryRun });
  if (command === "restart") {
    if (options.detach === true && !dryRun) {
      // A restart stops the tool room that started this process; run it in its own session.
      if (typeof options.log !== "string") throw new Error("--detach requires --log=<file>");
      const log = fs.openSync(path.resolve(options.log), "a");
      const child = spawn(process.execPath, [__filename, ...argv.filter((arg) => arg !== "--detach")], { detached: true, stdio: ["ignore", log, log] });
      child.unref();
      return { detached: true, pid: child.pid, log: path.resolve(options.log) };
    }
    return restartRuntime(layout, home, { callerRun: typeof options.callerRun === "string" ? options.callerRun : null, dryRun,
      force: options.force === true, swap: options.swap === true, rollback: options.noRollback !== true });
  }
  throw Object.assign(new Error(USAGE), { usage: true });
}

module.exports = { bundles, buildInfo, swapBundles, rollbackBundles, control, isReady, blockingTurns, packageAction, restartRuntime, parseArgs, main };

if (require.main === module) {
  main(process.argv.slice(2)).then((result) => {
    console.log(JSON.stringify(result, null, 2));
    process.exitCode = result?.ok === false ? 1 : 0;
  }, (error) => { console.error(error.usage ? error.message : `safe-self-update: ${error.message}`); process.exitCode = 2; });
}
