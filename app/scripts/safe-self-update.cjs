#!/usr/bin/env node
const crypto = require("node:crypto");
const fs = require("node:fs");
const net = require("node:net");
const os = require("node:os");
const path = require("node:path");
const readline = require("node:readline");
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

async function readClientTurns(bundle, home) {
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
    const chats = status.chats.filter((chat) => runIds.includes(chat.run_id))
      .map((chat) => ({ run_id: chat.run_id, turn_open: chat.turn_open === true, turn_state: chat.turn_state }));
    return { chats, clientRunIds: runIds };
  } finally { await store.close?.(); }
}

const readChatTurns = async (bundle, home) => (await readClientTurns(bundle, home)).chats;

// A run without a client chat surface (a service, memory or sensor-triggered
// run) has no projected turn status, so its turn frontier is folded from the
// movement ledger: the provider's turn/start dispatch opens a turn and the
// provider's terminal artifacts close it. A dispatch whose method cannot be
// read counts as an open turn. Rows recorded before the running runtime
// started belong to an instance whose turns the restart already stopped.
const TURN_DISPATCH_LABEL = "provider.transport.dispatch";
const TURN_CLOSING_LABELS = ["turn.completed", "turn.interrupted.result", "turn.error", "interface.artifact.output"];
const TURN_FRONTIER_NEEDLES = [TURN_DISPATCH_LABEL, ...TURN_CLOSING_LABELS].map((label) => `"${label}:`);

function dispatchMethod(blobs, content) {
  const read = (name) => JSON.parse(fs.readFileSync(path.join(blobs, name), "utf8"));
  try {
    if (!Array.isArray(content) || content.length !== 32) return null;
    let payload = read(Buffer.from(content).toString("hex"));
    if (typeof payload?.method !== "string" && /^[0-9a-f]{64}$/.test(payload?.payload_content_hash ?? "")) payload = read(payload.payload_content_hash);
    return typeof payload?.method === "string" ? payload.method : null;
  } catch { return null; }
}

async function readLedgerTurns(home, { sinceUnixMs = 0, skipRuns = [] } = {}) {
  const ledger = path.join(home, "ledger", "movements.jsonl"), blobs = path.join(home, "ledger", "blobs");
  if (!fs.statSync(ledger, { throwIfNoEntry: false })?.isFile()) return [];
  const skipped = new Set(skipRuns), open = new Map();
  const lines = readline.createInterface({ input: fs.createReadStream(ledger), crlfDelay: Infinity });
  for await (const line of lines) {
    if (!TURN_FRONTIER_NEEDLES.some((needle) => line.includes(needle))) continue;
    let movement;
    try { movement = JSON.parse(line); } catch { continue; }
    const run = movement?.invocation_metadata?.runtime?.run, id = movement?.artifact?.id;
    if (movement?.kind !== "InterfaceArrival" || typeof run !== "string" || !run || skipped.has(run) || typeof id !== "string") continue;
    if (Number.isFinite(movement.recorded_at_unix_ms) && movement.recorded_at_unix_ms < sinceUnixMs) continue;
    const label = id.slice(0, id.lastIndexOf(":"));
    if (TURN_CLOSING_LABELS.includes(label)) { open.delete(run); continue; }
    if (label !== TURN_DISPATCH_LABEL) continue;
    const method = dispatchMethod(blobs, movement.artifact.content);
    if (method !== null && method !== "turn/start") continue;
    open.set(run, { run_id: run, turn_open: true, turn_state: "working", source: "ledger",
      opened_movement_seq: Number.isSafeInteger(movement.seq) ? movement.seq : null });
  }
  return [...open.values()];
}

// Every turn a restart would stop: client chat runs from the client projection
// and every other run from the ledger.
async function readTurnsInFlight(bundle, home, { sinceUnixMs = 0 } = {}) {
  const { chats, clientRunIds } = await readClientTurns(bundle, home);
  const background = await readLedgerTurns(home, { sinceUnixMs, skipRuns: clientRunIds });
  return [...chats.map((chat) => ({ ...chat, source: "client" })), ...background];
}

function blockingTurns(turns, allowed) {
  const exempt = new Set((Array.isArray(allowed) ? allowed : [allowed]).filter((run) => typeof run === "string" && run));
  return turns.filter((turn) => turn.turn_open && !exempt.has(turn.run_id));
}

// A failure is identified by the interface that failed, never by how many
// interfaces fail: an interface that was healthy before a restart and fails
// after it is a regression even when another one recovered meanwhile.
const failureIdentity = (failure) => (typeof failure?.interface === "string" && failure.interface ? failure.interface : JSON.stringify(failure ?? null));
function newInterfaceFailures(before, after) {
  const known = new Set((Array.isArray(before?.interface_failures) ? before.interface_failures : []).map(failureIdentity));
  return (Array.isArray(after?.interface_failures) ? after.interface_failures : []).map(failureIdentity).filter((identity) => !known.has(identity));
}

// A bench receipt is evidence about one commit, measured on another machine.
// Its digest covers the producer's canonical form: the receipt without
// `digests`, keys sorted by code point, no whitespace, every character outside
// printable ASCII escaped as \uXXXX, and every number spelled as the file
// spells it (100.0 stays 100.0). JSON.parse loses that spelling, so the reader
// below keeps each number's text.
const RECEIPT_SCHEMA = "octopus.linux_bench_receipt.v1";
const RECEIPT_MAX_BYTES = 1024 * 1024;
const RECEIPT_FAILURE_SETS = [["rust_workspace", "failed_tests"], ["node_contracts", "failed_suites"], ["python_sensors_verifiers", "failed_files"]];
const RECEIPT_STRING_ESCAPES = { "\"": "\\\"", "\\": "\\\\", "\n": "\\n", "\r": "\\r", "\t": "\\t", "\b": "\\b", "\f": "\\f" };

function readStrictJson(text) {
  let at = 0;
  const fail = (what) => { throw new Error(`not strict JSON: ${what} at offset ${at}`); };
  const space = () => { while (at < text.length && " \t\n\r".includes(text[at])) at += 1; };
  const number = /-?(?:0|[1-9]\d*)(?:\.\d+)?(?:[eE][+-]?\d+)?/y;
  const string = () => {
    const start = at;
    for (at += 1; text[at] !== "\""; at += text[at] === "\\" ? 2 : 1) if (at >= text.length) fail("unterminated string");
    at += 1;
    try { return JSON.parse(text.slice(start, at)); } catch { return fail("malformed string"); }
  };
  const value = (depth) => {
    if (depth > 64) fail("nesting too deep");
    space();
    const first = text[at];
    if (first === "\"") return { string: string() };
    if (first === "{" || first === "[") {
      const close = first === "{" ? "}" : "]", entries = [], keys = new Set();
      const container = () => (first === "{" ? { members: entries } : { items: entries });
      at += 1;
      space();
      if (text[at] === close) { at += 1; return container(); }
      for (;;) {
        if (first === "{") {
          space();
          if (text[at] !== "\"") fail("expected a key");
          const key = string();
          if (keys.has(key)) fail(`duplicate key ${JSON.stringify(key)}`);
          keys.add(key);
          space();
          if (text[at] !== ":") fail("expected ':'");
          at += 1;
          entries.push([key, value(depth + 1)]);
        } else entries.push(value(depth + 1));
        space();
        if (text[at] === close) { at += 1; return container(); }
        if (text[at] !== ",") fail(`expected ',' or '${close}'`);
        at += 1;
      }
    }
    for (const word of ["true", "false", "null"]) if (text.startsWith(word, at)) { at += word.length; return { literal: word }; }
    number.lastIndex = at;
    const match = number.exec(text);
    if (!match) fail("unexpected token");
    at += match[0].length;
    return { literal: match[0] };
  };
  const root = value(0);
  space();
  if (at !== text.length) fail("trailing content");
  return root;
}

function codePointOrder(left, right) {
  const a = Array.from(left), b = Array.from(right);
  for (let index = 0; index < a.length && index < b.length; index += 1) {
    const difference = a[index].codePointAt(0) - b[index].codePointAt(0);
    if (difference) return difference;
  }
  return a.length - b.length;
}

const canonicalString = (value) => `"${value.replace(/["\\]|[^ -~]/g,
  (unit) => RECEIPT_STRING_ESCAPES[unit] ?? `\\u${unit.charCodeAt(0).toString(16).padStart(4, "0")}`)}"`;

function canonicalJson(node, omit = []) {
  if (node.members) {
    return `{${node.members.filter(([key]) => !omit.includes(key)).sort(([a], [b]) => codePointOrder(a, b))
      .map(([key, member]) => `${canonicalString(key)}:${canonicalJson(member)}`).join(",")}}`;
  }
  if (node.items) return `[${node.items.map((item) => canonicalJson(item)).join(",")}]`;
  return node.string === undefined ? node.literal : canonicalString(node.string);
}

function loadReceipt(file) {
  const stat = fs.statSync(file, { throwIfNoEntry: false });
  if (!stat?.isFile()) throw new Error("no such file");
  if (stat.size > RECEIPT_MAX_BYTES) throw new Error(`larger than ${RECEIPT_MAX_BYTES} bytes`);
  const text = fs.readFileSync(file, "utf8"), tree = readStrictJson(text);
  if (!tree.members) throw new Error("not a JSON object");
  const receipt = JSON.parse(text);
  if (receipt.schema !== RECEIPT_SCHEMA) throw new Error(`schema is ${JSON.stringify(receipt.schema ?? null)}, not ${RECEIPT_SCHEMA}`);
  const recorded = receipt.digests?.sha256;
  if (typeof recorded !== "string" || !/^[0-9a-f]{64}$/.test(recorded)) throw new Error("digests.sha256 is absent or malformed");
  const sha256 = crypto.createHash("sha256").update(canonicalJson(tree, ["digests"]), "utf8").digest("hex");
  if (sha256 !== recorded) throw new Error(`sha256 mismatch: recorded ${recorded}, recomputed ${sha256}`);
  return { receipt, sha256 };
}

// The gate a bundle passes before it is adopted. The candidate receipt must be
// intact, measured at the bundle's own revision, and record a clean cargo
// check. With a reference receipt, every failure the candidate records must be
// a failure the reference records too: failures are compared by name, so an
// equal count that hides a new failure does not pass.
function receiptGate(bundle, { requireReceipt = null, referenceReceipt = null } = {}) {
  if (!requireReceipt) {
    if (referenceReceipt) throw Object.assign(new Error("--reference-receipt requires --require-receipt"), { usage: true });
    return null;
  }
  const problems = [], newFailures = {};
  const load = (role, file) => {
    try { return { path: file, ...loadReceipt(file) }; } catch (error) { problems.push(`${role} receipt ${file}: ${error.message}`); return null; }
  };
  const candidate = load("candidate", requireReceipt), reference = referenceReceipt ? load("reference", referenceReceipt) : null;
  const revision = buildInfo(bundle)?.revision ?? null;
  if (candidate) {
    if (typeof revision !== "string" || candidate.receipt.commit !== revision) {
      problems.push(`candidate receipt is for commit ${JSON.stringify(candidate.receipt.commit ?? null)}; the bundle is ${JSON.stringify(revision)}`);
    }
    const rc = candidate.receipt.metrics?.cargo_check?.rc;
    if (rc !== 0) problems.push(`candidate receipt records cargo check rc ${JSON.stringify(rc ?? null)}`);
  }
  if (candidate && reference) {
    if (candidate.receipt.os_arch !== reference.receipt.os_arch) {
      problems.push(`candidate was measured on ${JSON.stringify(candidate.receipt.os_arch ?? null)}, the reference on ${JSON.stringify(reference.receipt.os_arch ?? null)}`);
    }
    const names = (list) => Array.isArray(list) && list.every((name) => typeof name === "string");
    for (const [suite, field] of RECEIPT_FAILURE_SETS) {
      const mine = candidate.receipt.metrics?.[suite]?.[field], theirs = reference.receipt.metrics?.[suite]?.[field];
      if (!names(mine) || !names(theirs)) {
        problems.push(`${names(mine) ? "reference" : "candidate"} receipt has no list of names at metrics.${suite}.${field}`);
        continue;
      }
      const known = new Set(theirs), added = [...new Set(mine)].filter((name) => !known.has(name));
      newFailures[suite] = added;
      if (added.length) problems.push(`${suite} has ${added.length} failure(s) the reference does not have: ${added.join(", ")}`);
    }
  }
  const describe = (loaded) => loaded && { path: loaded.path, commit: loaded.receipt.commit ?? null, sha256: loaded.sha256, timestamp_utc: loaded.receipt.timestamp_utc ?? null };
  const ok = problems.length === 0;
  return { ok, detail: ok ? `receipt ${candidate.sha256} for ${revision}${reference ? `; no failure beyond reference ${reference.sha256}` : ""}` : problems.join("; "),
    bundle_revision: revision, candidate: describe(candidate), reference: describe(reference), compared: Boolean(referenceReceipt), new_failures: newFailures };
}

const receiptRefusal = (gate, extra = {}) => ({ ok: false, applied: false, refused: "receipt_gate", detail: gate.detail, new_failures: gate.new_failures, receipt_gate: gate, ...extra });

const lastLines = (result) => `${result.stdout ?? ""}${result.stderr ?? ""}`.trim().split("\n").slice(-3).join(" | ");

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

async function packageStatus(bundle, home) {
  const catalog = await localPackages(bundle).inspectLocalPackageCatalog(packageRoots(bundle, home));
  return catalog.packages.filter((entry) => entry.available === false || entry.materialized || entry.partially_materialized || entry.installed).map((entry) => ({
    id: entry.id, action: packageAction(entry), receipt_state: entry.receipt_state ?? "", update_available: entry.update_available === true,
    local_changes: (entry.local_changes ?? []).length, error: entry.error || "",
    expected: { sourceDigest: entry.source_digest, installedDigest: entry.installed_digest, receiptState: entry.receipt_state, receiptDigest: entry.receipt_digest || null },
  }));
}

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

async function verifyStaging(layout, home, { smoke = true, allowModifiedSource = false, smokePackages = [CHAT_CLIENT_PACKAGE, "world_provider_codex"],
  requireReceipt = null, referenceReceipt = null } = {}) {
  const bundle = requireBundle(layout.staging), checks = [];
  const gate = receiptGate(bundle, { requireReceipt, referenceReceipt });
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
  if (gate) check("receipt_gate", gate.ok, gate.detail);
  if (smoke) checks.push(...await smokeTest(bundle, smokePackages));
  return { ok: checks.every((row) => row.ok), bundle, build: info, checks, ...(gate ? { receipt_gate: gate } : {}) };
}

async function restartRuntime(layout, home, options = {}) {
  const { callerRun = null, allowInFlightRuns = [], dryRun = false, force = false, swap = false, rollback = true,
    timeoutMs = 300_000, readTurns = readTurnsInFlight, requireReceipt = null, referenceReceipt = null } = options;
  const before = await control(home, "status");
  if (before.outcome !== "status") throw new Error(`no runtime answers for Home ${home} (${before.error}); only a running runtime is restarted`);
  if (swap) requireBundle(layout.staging);
  // The receipt speaks for the bundle the successor runs: staging when it is swapped in, otherwise the live one.
  const gate = receiptGate(swap ? layout.staging : layout.live, { requireReceipt, referenceReceipt });
  if (gate && !gate.ok) return receiptRefusal(gate);
  const blocking = blockingTurns(await readTurns(layout.live, home, { sinceUnixMs: before.started_at_unix_ms }), [callerRun, ...allowInFlightRuns]);
  const plan = [...(swap ? swapBundles(layout, { dryRun: true }).plan : []), "restart through the control socket", "wait for a ready successor",
    rollback ? `on failure: restore ${layout.prev} and restart again` : "on failure: report only"];
  if (blocking.length && !force) return { ok: false, applied: false, refused: "turns_in_flight", blocking, plan };
  if (dryRun) return { ok: true, applied: false, blocking, plan, runtime: before };
  if (swap) swapBundles(layout);
  const healthy = (status) => status.outcome === "status" && status.phase === "ready" && status.stopping === null
    && Array.isArray(status.interface_failures) && newInterfaceFailures(before, status).length === 0;
  await control(home, "restart");
  const first = await waitForStatus(home, (status) => healthy(status) && status.started_at_unix_ms !== before.started_at_unix_ms, timeoutMs);
  if (first.ok) return { ok: true, applied: true, swapped: swap, previous: before, runtime: first.status, revision: buildInfo(layout.live)?.revision ?? null };
  const regressed = newInterfaceFailures(before, first.status);
  if (!rollback || !fs.existsSync(layout.prev)) return { ok: false, applied: true, swapped: swap, rolled_back: false, new_interface_failures: regressed, runtime: first.status };
  rollbackBundles(layout);
  const current = await control(home, "status");
  if (current.outcome !== "status") {
    return { ok: false, applied: true, swapped: swap, rolled_back: true, recovered: false, new_interface_failures: regressed, failed: first.status,
      error: `no runtime answers for Home ${home} after the rollback; start it from ${layout.live}` };
  }
  await control(home, "restart");
  const second = await waitForStatus(home, (status) => healthy(status) && status.started_at_unix_ms !== before.started_at_unix_ms
    && status.started_at_unix_ms !== current.started_at_unix_ms, timeoutMs);
  return { ok: false, applied: true, swapped: swap, rolled_back: true, recovered: second.ok, new_interface_failures: regressed, failed: first.status, runtime: second.status };
}

async function status(layout, home) {
  const describe = (bundle) => (fs.existsSync(bundle) ? buildInfo(bundle) : null);
  const runtime = await control(home, "status");
  let turns = null;
  try {
    turns = (await readTurnsInFlight(layout.live, home, { sinceUnixMs: runtime.outcome === "status" ? runtime.started_at_unix_ms : 0 }))
      .filter((turn) => turn.turn_open);
  } catch (error) { turns = { error: error.message }; }
  return { home, head: spawnSync("git", ["rev-parse", "HEAD"], { cwd: repoRoot, encoding: "utf8" }).stdout.trim(), runtime,
    bundles: { live: describe(layout.live), staging: describe(layout.staging), prev: describe(layout.prev) }, turns_in_flight: turns };
}

const USAGE = `Usage: node app/scripts/safe-self-update.cjs <command> [options]
  status                          live runtime, bundle revisions, turns in flight (every run)
  build [--dry-run]               package into app/build/Octopus.staging.app
  verify [--no-smoke] [--allow-modified-source] [receipt options]
  packages status | packages sync [--dry-run] [--with-chat-client] [--only=a,b]
  swap [--dry-run] [receipt options]
                                  Octopus.app -> Octopus.prev.app, staging -> Octopus.app
  restart [--swap] [--caller-run=<run id>] [--allow-in-flight-run <run id>]... [--dry-run] [--force] [--no-rollback] [--detach --log=<file>]
          [receipt options]       refuses while any run other than the caller and the allowed ones has a turn in flight
  rollback [--dry-run]            Octopus.prev.app -> Octopus.app
Receipt options (verify, swap, restart):
  --require-receipt=<file>        an ${RECEIPT_SCHEMA} file: its SHA-256 must match its content (its BLAKE3 digest is not
                                  checked), its commit must be the bundle's revision and its cargo check must have passed;
                                  verify records the outcome as the receipt_gate check, swap and restart refuse before
                                  touching a bundle (restart without --swap checks the live bundle)
  --reference-receipt=<file>      with --require-receipt: every failed test, suite and file the required receipt names
                                  must also be named by this one
Common: --home=<Octopus Home, default $OCTOPUS_HOME or ~/.octopus> --build-root=<dir>`;

// Options that take a value, either as --name=value or as the next argument, and may repeat.
const REPEATABLE_VALUE_OPTIONS = new Set(["allow-in-flight-run"]);
// Options that take one file, either as --name=file or as the next argument.
const FILE_OPTIONS = new Set(["require-receipt", "reference-receipt"]);
const RECEIPT_COMMANDS = new Set(["verify", "swap", "restart"]);

function parseArgs(argv) {
  const positional = [], options = {};
  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (!arg.startsWith("--")) { positional.push(arg); continue; }
    const [name, ...rest] = arg.slice(2).split("=");
    const key = name.replace(/-([a-z])/g, (_, letter) => letter.toUpperCase());
    if (REPEATABLE_VALUE_OPTIONS.has(name)) {
      const value = rest.length ? rest.join("=") : argv[++index];
      if (typeof value !== "string" || !value || value.startsWith("--")) throw Object.assign(new Error(`--${name} requires a run id`), { usage: true });
      (options[key] ??= []).push(value);
      continue;
    }
    if (FILE_OPTIONS.has(name)) {
      const value = rest.length ? rest.join("=") : argv[++index];
      if (typeof value !== "string" || !value || value.startsWith("--")) throw Object.assign(new Error(`--${name} requires a file`), { usage: true });
      if (key in options) throw Object.assign(new Error(`--${name} is given more than once`), { usage: true });
      options[key] = value;
      continue;
    }
    options[key] = rest.length ? rest.join("=") : true;
  }
  return { positional, options };
}

async function main(argv) {
  const { positional, options } = parseArgs(argv);
  const layout = bundles(options.buildRoot ? path.resolve(options.buildRoot) : undefined);
  const home = path.resolve(options.home || process.env.OCTOPUS_HOME || path.join(os.homedir(), ".octopus"));
  const dryRun = options.dryRun === true, [command, sub] = positional;
  const receipts = { requireReceipt: options.requireReceipt ? path.resolve(options.requireReceipt) : null,
    referenceReceipt: options.referenceReceipt ? path.resolve(options.referenceReceipt) : null };
  if (receipts.referenceReceipt && !receipts.requireReceipt) throw Object.assign(new Error("--reference-receipt requires --require-receipt"), { usage: true });
  if (receipts.requireReceipt && !RECEIPT_COMMANDS.has(command)) throw Object.assign(new Error("--require-receipt applies to verify, swap and restart"), { usage: true });
  if (command === "status") return status(layout, home);
  if (command === "build") return buildStaging(layout, { dryRun });
  if (command === "verify") return verifyStaging(layout, home, { smoke: options.noSmoke !== true, allowModifiedSource: options.allowModifiedSource === true, ...receipts });
  if (command === "packages" && sub === "status") return { packages: await packageStatus(layout.live, home), baseline: baselineDrift(layout.live, home) };
  if (command === "packages" && sub === "sync") {
    return syncPackages(layout.live, home, { dryRun, withChatClient: options.withChatClient === true, only: typeof options.only === "string" ? options.only.split(",") : null });
  }
  if (command === "swap") {
    const gate = receipts.requireReceipt ? receiptGate(requireBundle(layout.staging), receipts) : null;
    if (gate && !gate.ok) return receiptRefusal(gate, { plan: swapBundles(layout, { dryRun: true }).plan });
    return { ...swapBundles(layout, { dryRun }), ...(gate ? { receipt_gate: gate } : {}) };
  }
  if (command === "rollback") return rollbackBundles(layout, { dryRun });
  if (command === "restart") {
    if (options.detach === true && !dryRun) {
      if (typeof options.log !== "string") throw new Error("--detach requires --log=<file>");
      // The detached child checks the gate again; the caller learns of a refusal here.
      const gate = receiptGate(options.swap === true ? requireBundle(layout.staging) : layout.live, receipts);
      if (gate && !gate.ok) return receiptRefusal(gate);
      const log = fs.openSync(path.resolve(options.log), "a");
      const child = spawn(process.execPath, [__filename, ...argv.filter((arg) => arg !== "--detach")], { detached: true, stdio: ["ignore", log, log] });
      child.unref();
      return { detached: true, pid: child.pid, log: path.resolve(options.log) };
    }
    return restartRuntime(layout, home, { callerRun: typeof options.callerRun === "string" ? options.callerRun : null, dryRun,
      allowInFlightRuns: Array.isArray(options.allowInFlightRun) ? options.allowInFlightRun : [],
      force: options.force === true, swap: options.swap === true, rollback: options.noRollback !== true, ...receipts });
  }
  throw Object.assign(new Error(USAGE), { usage: true });
}

module.exports = { bundles, buildInfo, swapBundles, rollbackBundles, control, isReady, readChatTurns, readLedgerTurns, readTurnsInFlight, blockingTurns,
  newInterfaceFailures, packageAction, restartRuntime, parseArgs, main, readStrictJson, canonicalJson, loadReceipt, receiptGate, verifyStaging };

if (require.main === module) {
  main(process.argv.slice(2)).then((result) => {
    console.log(JSON.stringify(result, null, 2));
    process.exitCode = result?.ok === false ? 1 : 0;
  }, (error) => { console.error(error.usage ? error.message : `safe-self-update: ${error.message}`); process.exitCode = 2; });
}
