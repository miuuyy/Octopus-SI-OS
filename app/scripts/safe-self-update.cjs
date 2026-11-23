#!/usr/bin/env node
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

async function waitForStatus(home, accept, timeoutMs, pollMs = 250) {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    const status = await control(home, "status", 2000);
    if (accept(status)) return { ok: true, status };
    if (Date.now() >= deadline) return { ok: false, status };
    await delay(pollMs);
  }
}

const DEFAULT_LAUNCHD_SERVICE_LABEL = "app.octopus.runtime";
const DEFAULT_TERMINATE_GRACE_MS = 5_000;
const DEFAULT_TERMINATE_POLL_MS = 100;

function normalizeFsPath(targetPath) {
  const resolved = path.resolve(targetPath);
  try { return fs.realpathSync(resolved); } catch {}
  const missing = [];
  let cursor = resolved;
  while (cursor !== path.dirname(cursor)) {
    missing.unshift(path.basename(cursor));
    cursor = path.dirname(cursor);
    try { return path.join(fs.realpathSync(cursor), ...missing); } catch {}
  }
  return resolved;
}

function readRuntimeInstanceState(home) {
  const file = path.join(home, "state", "runtime-instance", "state.json");
  try {
    const record = JSON.parse(fs.readFileSync(file, "utf8"));
    if (!Number.isSafeInteger(record?.pid) || record.pid <= 0) return null;
    return {
      pid: record.pid,
      phase: typeof record.phase === "string" ? record.phase : null,
      started_at_unix_ms: sequence(record.started_at_unix_ms) ? record.started_at_unix_ms : null,
    };
  } catch {
    return null;
  }
}

function defaultIsProcessAlive(pid) {
  if (!Number.isSafeInteger(pid) || pid <= 1) return false;
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
}

function defaultSendSignal(pid, signal) {
  try {
    process.kill(pid, signal);
    return { delivered: true, code: null, error: null };
  } catch (error) {
    return { delivered: false, code: error?.code ?? null, error: error?.message ?? String(error) };
  }
}

function defaultLaunchctl(args) {
  const result = spawnSync("launchctl", args, { encoding: "utf8", timeout: 15_000 });
  return {
    status: result.status,
    stdout: result.stdout ?? "",
    stderr: result.stderr ?? "",
    error: result.error ? result.error.message : null,
  };
}

function parseLaunchdService(stdout) {
  const text = typeof stdout === "string" ? stdout : "";
  const matchField = (pattern) => {
    const match = pattern.exec(text);
    return match ? match[1].trim() : null;
  };
  const pidText = matchField(/^\s*pid\s*=\s*(\d+)\s*$/m);
  return {
    state: matchField(/^\s*state\s*=\s*(.+?)\s*$/m),
    program: matchField(/^\s*program\s*=\s*(.+?)\s*$/m),
    octopusHome: matchField(/^\s*OCTOPUS_HOME\s*=>\s*(.+?)\s*$/m),
    pid: pidText ? Number(pidText) : null,
  };
}

function inspectLaunchdService(layout, home, {
  platform = process.platform,
  uid = typeof process.getuid === "function" ? process.getuid() : null,
  launchdServiceLabel = DEFAULT_LAUNCHD_SERVICE_LABEL,
  launchctl = defaultLaunchctl,
} = {}) {
  if (platform !== "darwin") return { loaded: false, matchesTarget: false, target: null, reason: "non_darwin" };
  if (!Number.isSafeInteger(uid) || uid < 0 || !nonempty(launchdServiceLabel)) {
    return { loaded: false, matchesTarget: false, target: null, reason: "invalid_target" };
  }
  const target = `gui/${uid}/${launchdServiceLabel}`;
  const result = launchctl(["print", target]);
  if (!result || result.status !== 0) {
    return {
      loaded: false,
      matchesTarget: false,
      target,
      reason: result?.error ? `launchctl_error: ${result.error}` : "not_loaded",
    };
  }
  const parsed = parseLaunchdService(result.stdout);
  const expectedHome = normalizeFsPath(home);
  const expectedBinary = normalizeFsPath(runtimeBinary(layout.live));
  if (parsed.octopusHome && normalizeFsPath(parsed.octopusHome) !== expectedHome) {
    return {
      loaded: true,
      matchesTarget: false,
      target,
      ...parsed,
      reason: `launchd service OCTOPUS_HOME (${parsed.octopusHome}) does not match ${home}`,
    };
  }
  if (parsed.program && normalizeFsPath(parsed.program) !== expectedBinary) {
    return {
      loaded: true,
      matchesTarget: false,
      target,
      ...parsed,
      reason: `launchd service program (${parsed.program}) does not match ${runtimeBinary(layout.live)}`,
    };
  }
  return { loaded: true, matchesTarget: true, target, ...parsed, reason: null };
}

async function terminateProcess(pid, {
  isProcessAlive = defaultIsProcessAlive,
  sendSignal = defaultSendSignal,
  graceMs = DEFAULT_TERMINATE_GRACE_MS,
  pollMs = DEFAULT_TERMINATE_POLL_MS,
} = {}) {
  if (!Number.isSafeInteger(pid) || pid <= 1 || pid === process.pid) {
    return { pid, alive: false, terminated: false, signals: [], skipped: "invalid_pid" };
  }
  if (!isProcessAlive(pid)) {
    return { pid, alive: false, terminated: false, signals: [] };
  }
  const signals = [];
  const waitGone = async (budgetMs) => {
    const deadline = Date.now() + budgetMs;
    for (;;) {
      if (!isProcessAlive(pid)) return true;
      const remaining = deadline - Date.now();
      if (remaining <= 0) return !isProcessAlive(pid);
      await delay(Math.min(pollMs, remaining));
    }
  };
  const term = sendSignal(pid, "SIGTERM");
  signals.push("SIGTERM");
  if (!term.delivered) {
    if (term.code === "ESRCH") return { pid, alive: false, terminated: true, signals };
    return { pid, alive: isProcessAlive(pid), terminated: false, signals, error: term.error };
  }
  if (await waitGone(graceMs)) return { pid, alive: false, terminated: true, signals };
  const kill = sendSignal(pid, "SIGKILL");
  signals.push("SIGKILL");
  if (!kill.delivered) {
    if (kill.code === "ESRCH") return { pid, alive: false, terminated: true, signals };
    return { pid, alive: isProcessAlive(pid), terminated: false, signals, error: kill.error };
  }
  const gone = await waitGone(Math.max(pollMs, Math.min(graceMs, 2_000)));
  return { pid, alive: !gone, terminated: gone, signals };
}

function collectCandidatePids({ current, second, first, instanceState, launchdInfo, before }) {
  const pids = [];
  const add = (pid) => {
    if (Number.isSafeInteger(pid) && pid > 1 && pid !== process.pid && !pids.includes(pid)) {
      pids.push(pid);
    }
  };
  if (second?.status?.outcome === "status") add(second.status.pid);
  if (current?.outcome === "status") add(current.pid);
  if (first?.status?.outcome === "status") add(first.status.pid);
  if (instanceState?.pid) add(instanceState.pid);
  if (launchdInfo?.matchesTarget && launchdInfo.pid) add(launchdInfo.pid);
  if (before?.pid) add(before.pid);
  return pids;
}

async function recoverRolledBackRuntime(layout, home, { before, first, current, second, staleStartedAt, healthy, options }) {
  const {
    timeoutMs = 300_000,
    recoveryTimeoutMs = timeoutMs,
    terminateGraceMs = DEFAULT_TERMINATE_GRACE_MS,
    terminatePollMs = DEFAULT_TERMINATE_POLL_MS,
    statusPollMs = 250,
    isProcessAlive = defaultIsProcessAlive,
    sendSignal = defaultSendSignal,
    launchctl = defaultLaunchctl,
    platform = process.platform,
    uid = typeof process.getuid === "function" ? process.getuid() : null,
    launchdServiceLabel = DEFAULT_LAUNCHD_SERVICE_LABEL,
  } = options;
  const graceMs = nonNegativeMs(terminateGraceMs, "terminateGraceMs");
  const termPollMs = positiveMs(terminatePollMs, "terminatePollMs");
  const pollMs = positiveMs(statusPollMs, "statusPollMs");
  const instanceState = readRuntimeInstanceState(home);
  const launchd = inspectLaunchdService(layout, home, { platform, uid, launchdServiceLabel, launchctl });
  const candidatePids = collectCandidatePids({ current, second, first, instanceState, launchdInfo: launchd, before });
  const terminations = [];
  for (const pid of candidatePids) {
    if (!isProcessAlive(pid)) continue;
    terminations.push(await terminateProcess(pid, { isProcessAlive, sendSignal, graceMs, pollMs: termPollMs }));
  }
  const terminatedPids = terminations.filter((entry) => entry.terminated).map((entry) => entry.pid);
  const stillAlivePids = terminations.filter((entry) => entry.alive).map((entry) => entry.pid);
  const isRecovered = (status) => healthy(status) && sequence(status.started_at_unix_ms) && !staleStartedAt.has(status.started_at_unix_ms);

  const afterTerminate = await control(home, "status", 2000);
  if (isRecovered(afterTerminate)) {
    return {
      recovered: true,
      runtime: afterTerminate,
      recovery: {
        terminated_pids: terminatedPids,
        ...(stillAlivePids.length ? { still_alive_pids: stillAlivePids } : {}),
        terminations,
        launchd_target: launchd.target,
        launchd_loaded: launchd.loaded && launchd.matchesTarget,
        launchd_kickstarted: false,
      },
    };
  }

  if (launchd.loaded && launchd.matchesTarget) {
    const kick = launchctl(["kickstart", "-k", launchd.target]);
    const kickstarted = kick?.status === 0;
    if (!kickstarted) {
      const detail = kick?.error || lastLines(kick ?? {}) || `exit status ${kick?.status}`;
      return {
        recovered: false,
        runtime: afterTerminate,
        error: `launchctl kickstart -k ${launchd.target} failed after rollback: ${detail}`,
        recovery: {
          terminated_pids: terminatedPids,
          ...(stillAlivePids.length ? { still_alive_pids: stillAlivePids } : {}),
          terminations,
          launchd_target: launchd.target,
          launchd_loaded: true,
          launchd_kickstarted: false,
          launchd_error: detail,
        },
      };
    }
    const waited = await waitForStatus(home, isRecovered, recoveryTimeoutMs, pollMs);
    return {
      recovered: waited.ok,
      runtime: waited.status,
      ...(waited.ok ? {} : {
        error: `runtime did not become ready for Home ${home} after rollback and launchctl kickstart -k ${launchd.target}`,
      }),
      recovery: {
        terminated_pids: terminatedPids,
        ...(stillAlivePids.length ? { still_alive_pids: stillAlivePids } : {}),
        terminations,
        launchd_target: launchd.target,
        launchd_loaded: true,
        launchd_kickstarted: true,
      },
    };
  }

  return {
    recovered: false,
    runtime: afterTerminate,
    error: `no runtime answers for Home ${home} after the rollback${launchd.reason ? ` (launchd: ${launchd.reason})` : ""}; start it from ${layout.live}`,
    recovery: {
      terminated_pids: terminatedPids,
      ...(stillAlivePids.length ? { still_alive_pids: stillAlivePids } : {}),
      terminations,
      launchd_target: launchd.target,
      launchd_loaded: false,
      launchd_kickstarted: false,
      ...(launchd.reason ? { launchd_reason: launchd.reason } : {}),
    },
  };
}

function runtimeEnv(bundle, home) {
  const root = installation(bundle), user = os.userInfo().username;
  const codexHome = process.env.CODEX_HOME || path.join(os.homedir(), ".codex");
  return {
    HOME: os.homedir(), USER: user, LOGNAME: user, TMPDIR: os.tmpdir(),
    CODEX_HOME: codexHome,
    OCTOPUS_MACHINE_CODEX_AUTH_FILE: path.join(codexHome, "auth.json"),
    OCTOPUS_MACHINE_CODEX_AUTH_LOCK_FILE: path.join(codexHome, "auth.lock"),
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
    if (!Array.isArray(scope.client_runs) || scope.client_runs.some((row) => !nonempty(row?.run_id))) {
      throw new Error("client turn scope is unavailable");
    }
    const runIds = scope.client_runs.map((row) => row.run_id), scopedRuns = new Set(runIds);
    const status = await store.read(projectionReadRequest("projection.operator.chats.status", { run_ids: runIds }));
    if (!Array.isArray(status.chats)) throw new Error("client turn status is unavailable");
    const chats = status.chats.filter((chat) => scopedRuns.has(chat?.run_id))
      .map((chat) => {
        if (typeof chat.turn_open !== "boolean") throw new Error("client turn status is malformed");
        return { run_id: chat.run_id, turn_open: chat.turn_open, turn_state: chat.turn_state,
          opened_movement_seq: chat.turn_opened_movement_seq ?? null };
      });
    if (new Set(chats.map((chat) => chat.run_id)).size !== runIds.length || chats.length !== runIds.length) {
      throw new Error("client turn status does not cover the complete scope");
    }
    return { chats, clientRunIds: runIds };
  } finally { await store.close?.(); }
}

const readChatTurns = async (bundle, home) => (await readClientTurns(bundle, home)).chats;

const TURN_DISPATCH_LABEL = "provider.transport.dispatch";
const TURN_STARTED_LABEL = "turn.started";
const TURN_CLOSING_LABELS = ["turn.completed", "turn.interrupted.result", "turn.error", "interface.artifact.output"];
const nonempty = (value) => typeof value === "string" && value.length > 0;
const sequence = (value) => Number.isSafeInteger(value) && value >= 0;
const UNIT_MOVEMENT_KINDS = new Set(["ArtifactMoved", "VerificationFailed", "DeliveryRefused", "InterfaceArrival", "InterfaceOutbound"]);
function validMovementKind(kind) {
  if (typeof kind === "string") return UNIT_MOVEMENT_KINDS.has(kind);
  return kind !== null && typeof kind === "object" && !Array.isArray(kind)
    && Object.keys(kind).length === 1 && Object.hasOwn(kind, "MarkOutcomeAdmitted")
    && nonempty(kind.MarkOutcomeAdmitted);
}
const blobHex = (value) => Array.isArray(value) && value.length === 32
  && value.every((byte) => Number.isInteger(byte) && byte >= 0 && byte <= 255) ? Buffer.from(value).toString("hex") : null;
const artifactIdentity = (artifact) => {
  const hash = blobHex(artifact?.content);
  return hash && nonempty(artifact.id) ? JSON.stringify([artifact.id, hash]) : null;
};
const ledgerPath = (home) => path.join(home, "ledger", "movements.jsonl");
const fileStamp = (stat) => [stat.dev, stat.ino, stat.size, stat.mtimeNs, stat.ctimeNs].join(":");
const transientLedgerError = (message) => Object.assign(new Error(message), { transientLedgerChange: true });

function ledgerStamp(home) {
  let stat;
  try { stat = fs.lstatSync(ledgerPath(home), { bigint: true }); }
  catch (error) { throw new Error("movement ledger unavailable (" + (error.code ?? "stat failed") + ")"); }
  if (!stat.isFile()) throw new Error("movement ledger unavailable (not a regular file)");
  return fileStamp(stat);
}

function frontierPayload(blobs, artifact, label, seq) {
  const read = (hash) => JSON.parse(fs.readFileSync(path.join(blobs, hash), "utf8"));
  try {
    const hash = blobHex(artifact?.content);
    if (!hash) throw new Error();
    let payload = read(hash);
    if (Object.hasOwn(payload, "payload_content_hash")) {
      if (payload.label !== label || !/^[0-9a-f]{64}$/.test(payload.payload_content_hash)) throw new Error();
      payload = read(payload.payload_content_hash);
    }
    if (payload?.label !== label) throw new Error();
    return payload;
  } catch {
    throw new Error("unreadable " + label + " evidence at ledger sequence " + seq);
  }
}

function nativeTurn(payload) {
  const thread = payload.threadId ?? payload.thread_id, turn = payload.turn?.id ?? payload.turn_id;
  return nonempty(thread) && nonempty(turn) ? JSON.stringify([thread, turn]) : null;
}

async function readLedgerTurns(home, { sinceUnixMs = 0, skipRuns = [] } = {}) {
  const ledger = ledgerPath(home), blobs = path.join(home, "ledger", "blobs");
  const stamp = ledgerStamp(home), skip = new Set(skipRuns);
  const open = new Map(), sources = new Map(), usedInputs = new Map();
  let lastSeq = -1;
  function observe(line) {
    let movement;
    try { movement = JSON.parse(line); }
    catch { throw new Error("malformed movement ledger row after sequence " + lastSeq); }
    if (!sequence(movement?.seq) || movement.seq !== lastSeq + 1
        || !sequence(movement.recorded_at_unix_ms) || !validMovementKind(movement.kind)) {
      throw new Error("invalid or non-contiguous movement ledger row after sequence " + lastSeq);
    }
    lastSeq = movement.seq;
    const metadata = movement.invocation_metadata, runtime = metadata?.runtime, run = runtime?.run;
    const artifact = movement.artifact;
    if (movement.kind === "InterfaceOutbound" && nonempty(run) && !skip.has(run)
        && nonempty(runtime.interface) && movement.to?.Interface === runtime.interface && nonempty(movement.from?.Interface)) {
      const input = artifactIdentity(artifact);
      if (input) sources.set(lastSeq, { run, provider: runtime.interface, input });
    }
    if (movement.recorded_at_unix_ms < sinceUnixMs || movement.kind !== "InterfaceArrival") return;
    const provider = movement.from?.Interface;
    const id = typeof artifact?.id === "string" ? artifact.id : "";
    const delimiter = id.lastIndexOf(":");
    const label = delimiter < 0 ? id : id.slice(0, delimiter);
    const frontier = label === TURN_DISPATCH_LABEL || label === TURN_STARTED_LABEL || TURN_CLOSING_LABELS.includes(label);
    if (metadata?.arrival_fanout || metadata?.verification_failure) return;
    if (!nonempty(run) || !nonempty(provider) || runtime.interface !== provider || runtime.worker !== movement.to?.Interface) {
      if (frontier) throw new Error("missing provider/run provenance at ledger sequence " + lastSeq);
      return;
    }
    if (skip.has(run)) return;
    if (!frontier) return;
    if (delimiter < 0 || delimiter === id.length - 1) throw new Error("invalid frontier artifact id at ledger sequence " + lastSeq);
    const current = open.get(run) ?? [];
    if (label === TURN_DISPATCH_LABEL) {
      const payload = frontierPayload(blobs, artifact, label, lastSeq);
      if (!nonempty(payload.method) || payload.provider_interface !== provider
          || !sequence(payload.source_movement_seq) || payload.source_movement_seq >= lastSeq) {
        throw new Error("invalid provider dispatch identity at ledger sequence " + lastSeq);
      }
      if (payload.method !== "turn/start") return;
      const source = sources.get(payload.source_movement_seq);
      if (source?.run !== run || source.provider !== provider) {
        throw new Error("missing exact provider input for dispatch at ledger sequence " + lastSeq);
      }
      const sourceInput = source.input;
      const key = JSON.stringify([run, provider, sourceInput]);
      const count = (usedInputs.get(key) ?? 0) + 1;
      usedInputs.set(key, count);
      current.push({ provider, seq: lastSeq, sourceSeq: payload.source_movement_seq, input: sourceInput, key,
        native: null, ambiguous: null });
      open.set(run, current);
      return;
    }
    const candidates = current.filter((turn) => turn.provider === provider);
    if (!candidates.length) return;
    const payload = frontierPayload(blobs, artifact, label, lastSeq);
    if (label === "turn.error" && payload.will_retry !== false) return;
    const sourceSeq = payload.source_movement_seq ?? payload.additional_details?.source_movement_seq;
    const reply = artifactIdentity(payload.in_reply_to), native = nativeTurn(payload);
    const matches = candidates.filter((turn) => {
      if ((payload.in_reply_to != null && (!reply || turn.input !== reply))
          || (sourceSeq != null && turn.sourceSeq !== sourceSeq)
          || (native && turn.native && native !== turn.native)) return false;
      return (native && turn.native === native)
        || sourceSeq != null || (usedInputs.get(turn.key) === 1 && reply);
    });
    if (matches.length !== 1) {
      if (label !== TURN_STARTED_LABEL) {
        for (const turn of candidates) turn.ambiguous = lastSeq;
      }
      return;
    }
    const [turn] = matches;
    if (label === TURN_STARTED_LABEL) { if (native) turn.native = native; }
    else open.set(run, current.filter((entry) => entry !== turn));
  }

  let file;
  try {
    file = await fs.promises.open(ledger, fs.constants.O_RDONLY | fs.constants.O_NOFOLLOW | fs.constants.O_NONBLOCK);
    const stat = await file.stat({ bigint: true });
    if (fileStamp(stat) !== stamp) throw transientLedgerError("movement ledger changed before inspection");
    if (stat.size > BigInt(Number.MAX_SAFE_INTEGER)) throw new Error("movement ledger exceeds inspection range");
    const decoder = new TextDecoder("utf-8", { fatal: true });
    let pending = "";
    const bytesAtSnapshot = stat.size ? file.createReadStream({ autoClose: false, end: Number(stat.size) - 1 }) : [];
    for await (const bytes of bytesAtSnapshot) {
      const lines = (pending + decoder.decode(bytes, { stream: true })).split("\n");
      pending = lines.pop();
      for (const line of lines) observe(line);
      if (pending.length > 16 * 1024 * 1024) throw new Error("movement ledger row exceeds inspection limit");
    }
    const trailing = decoder.decode();
    if (ledgerStamp(home) !== stamp) throw transientLedgerError("movement ledger changed during inspection");
    if (trailing || pending) throw new Error("torn movement ledger row after sequence " + lastSeq);
  } catch (error) {
    if (error.transientLedgerChange) throw error;
    if (!error.code) {
      let changed = false;
      try { changed = ledgerStamp(home) !== stamp; } catch {}
      if (changed) throw transientLedgerError("movement ledger changed during inspection");
    }
    if (error.code) throw new Error("movement ledger unreadable (" + error.code + ")");
    throw error;
  } finally {
    await file?.close();
  }
  return [...open].flatMap(([run_id, turns]) => turns.map((turn) => {
    if (turn.ambiguous !== null) throw new Error("ambiguous turn closure at ledger sequence " + turn.ambiguous
      + " for dispatch " + turn.seq);
    return { run_id, turn_open: true, turn_state: "working", source: "ledger", opened_movement_seq: turn.seq };
  }));
}

async function readTurnsInFlight(bundle, home, { sinceUnixMs = 0 } = {}) {
  const { chats, clientRunIds } = await readClientTurns(bundle, home);
  const background = await readLedgerTurns(home, { sinceUnixMs, skipRuns: clientRunIds });
  return [...chats.map((chat) => ({ ...chat, source: "client" })), ...background];
}

function blockingTurns(turns, allowed) {
  const exempt = new Set((Array.isArray(allowed) ? allowed : [allowed]).filter((run) => typeof run === "string" && run));
  return turns.filter((turn) => turn.turn_open && !exempt.has(turn.run_id));
}

const failureIdentity = (failure) => (typeof failure?.interface === "string" && failure.interface ? failure.interface : JSON.stringify(failure ?? null));
function newInterfaceFailures(before, after) {
  const known = new Set((Array.isArray(before?.interface_failures) ? before.interface_failures : []).map(failureIdentity));
  return (Array.isArray(after?.interface_failures) ? after.interface_failures : []).map(failureIdentity).filter((identity) => !known.has(identity));
}

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

const turnIdentity = (turn) => JSON.stringify([turn.run_id, turn.source, turn.opened_movement_seq]);

const DEFAULT_WAIT_QUIET_MS = 300_000;
const DEFAULT_QUIET_WINDOW_MS = 10_000;
const DEFAULT_QUIET_POLL_MS = 2_000;

function nonNegativeMs(value, label) {
  if (!Number.isSafeInteger(value) || value < 0) throw new Error(`${label} must be a non-negative integer in milliseconds`);
  return value;
}

function positiveMs(value, label) {
  if (!Number.isSafeInteger(value) || value < 1) throw new Error(`${label} must be a positive integer in milliseconds`);
  return value;
}

async function restartRuntime(layout, home, options = {}) {
  const { callerRun = null, allowInFlightRuns = [], dryRun = false, force = false, swap = false, rollback = true,
    timeoutMs = 300_000, waitQuietMs = 0, quietWindowMs = waitQuietMs > 0 ? DEFAULT_QUIET_WINDOW_MS : 0,
    pollIntervalMs = DEFAULT_QUIET_POLL_MS, statusPollMs = 250, readTurns = readTurnsInFlight } = options;
  const waitBudgetMs = nonNegativeMs(waitQuietMs, "waitQuietMs");
  const settleMs = nonNegativeMs(quietWindowMs, "quietWindowMs");
  const pollMs = positiveMs(pollIntervalMs, "pollIntervalMs");
  const statusPoll = positiveMs(statusPollMs, "statusPollMs");
  const before = await control(home, "status");
  if (before?.outcome !== "status") throw new Error(`no runtime answers for Home ${home} (${before?.error}); only a running runtime is restarted`);
  if (swap) requireBundle(layout.staging);
  const plan = [...(swap ? swapBundles(layout, { dryRun: true }).plan : []), "restart through the control socket", "wait for a ready successor",
    rollback ? `on failure: restore ${layout.prev} and recover runtime` : "on failure: report only"];
  const refuse = (refused, extra = {}) => ({ ok: false, applied: false, refused, plan, ...extra });
  const usable = (status) => status?.outcome === "status" && status.phase === "ready" && status.stopping === null
    && sequence(status.started_at_unix_ms) && status.started_at_unix_ms > 0 && sequence(status.pid) && status.pid > 0
    && Array.isArray(status.interface_failures);
  if (!usable(before)) return refuse("runtime_not_ready");
  const checkRuntime = async () => {
    const now = await control(home, "status");
    if (now?.outcome !== "status") return refuse("runtime_unreachable", { error: now?.error });
    if (!usable(now) || now.started_at_unix_ms !== before.started_at_unix_ms || now.pid !== before.pid
        || newInterfaceFailures(before, now).length) {
      return refuse("runtime_changed");
    }
    return null;
  };
  const inspect = async () => {
    const stamp = readTurns === readTurnsInFlight ? ledgerStamp(home) : null;
    const turns = await readTurns(layout.live, home, { sinceUnixMs: before.started_at_unix_ms });
    if (stamp !== null && ledgerStamp(home) !== stamp) throw transientLedgerError("movement ledger changed during turn inspection");
    return { turns, stamp };
  };
  const allowed = [callerRun, ...allowInFlightRuns];
  const inspectFrontier = async () => {
    try {
      const first = await inspect();
      let blocking = blockingTurns(first.turns, allowed);
      if (blocking.length && !force) return { ok: false, retryable: true, refusal: refuse("turns_in_flight", { blocking }) };
      const changedBefore = await checkRuntime();
      if (changedBefore) return { ok: false, retryable: false, refusal: changedBefore };
      const last = await inspect();
      const seen = new Set(blockingTurns(first.turns, []).map(turnIdentity));
      const newlyOpen = blockingTurns(last.turns, []).filter((turn) => !seen.has(turnIdentity(turn)));
      blocking = blockingTurns(last.turns, allowed);
      if (newlyOpen.length || (blocking.length && !force)) {
        return { ok: false, retryable: true, refusal: refuse("turns_in_flight", { blocking: newlyOpen.length ? newlyOpen : blocking }) };
      }
      const changedAfter = await checkRuntime();
      if (changedAfter) return { ok: false, retryable: false, refusal: changedAfter };
      if (last.stamp !== null && (last.stamp !== first.stamp || ledgerStamp(home) !== last.stamp)) {
        return { ok: false, retryable: true, refusal: refuse("turn_frontier_changed") };
      }
      return { ok: true, blocking, stamp: last.stamp };
    } catch (error) {
      return {
        ok: false,
        retryable: error?.transientLedgerChange === true,
        refusal: refuse("turn_evidence_untrusted", { error: error.message }),
      };
    }
  };
  let blocking;
  let waitSummary = null;
  if (waitBudgetMs === 0) {
    const check = await inspectFrontier();
    if (!check.ok) return check.refusal;
    blocking = check.blocking;
  } else {
    const startedAt = Date.now(), deadline = startedAt + waitBudgetMs;
    let attempts = 0, quietSince = null, quietStamp = null, lastRefusal = null;
    for (;;) {
      attempts += 1;
      const check = await inspectFrontier();
      if (!check.ok) {
        quietSince = null;
        quietStamp = null;
        if (!check.retryable) return { ...check.refusal, waited_ms: Date.now() - startedAt, attempts };
        lastRefusal = check.refusal;
      } else if (settleMs === 0) {
        blocking = check.blocking;
        waitSummary = { waited_ms: Date.now() - startedAt, attempts };
        break;
      } else {
        const now = Date.now();
        if (quietSince === null || (check.stamp !== null && check.stamp !== quietStamp)) {
          quietSince = now;
          quietStamp = check.stamp;
          lastRefusal = refuse("turn_frontier_changed", { error: "waiting for quiet window to settle" });
        } else if (now - quietSince >= settleMs) {
          blocking = check.blocking;
          waitSummary = { waited_ms: now - startedAt, attempts };
          break;
        } else {
          lastRefusal = refuse("turn_frontier_changed", { error: "waiting for quiet window to settle" });
        }
      }
      const remaining = deadline - Date.now();
      if (remaining <= 0) {
        return { ...lastRefusal, timed_out: true, waited_ms: Date.now() - startedAt, attempts };
      }
      const untilSettle = quietSince !== null ? Math.max(1, settleMs - (Date.now() - quietSince)) : pollMs;
      await delay(Math.min(pollMs, remaining, untilSettle));
    }
  }
  if (dryRun) return { ok: true, applied: false, blocking, plan, runtime: before, ...(waitSummary ?? {}) };
  if (swap) swapBundles(layout);
  const healthy = (status) => status?.outcome === "status" && status.phase === "ready" && status.stopping === null
    && Array.isArray(status.interface_failures) && newInterfaceFailures(before, status).length === 0;
  await control(home, "restart");
  const first = await waitForStatus(home, (status) => healthy(status) && status.started_at_unix_ms !== before.started_at_unix_ms, timeoutMs, statusPoll);
  if (first.ok) return { ok: true, applied: true, swapped: swap, previous: before, runtime: first.status, revision: buildInfo(layout.live)?.revision ?? null, ...(waitSummary ?? {}) };
  const regressed = newInterfaceFailures(before, first.status);
  if (!rollback || !fs.existsSync(layout.prev)) return { ok: false, applied: true, swapped: swap, rolled_back: false, new_interface_failures: regressed, runtime: first.status, ...(waitSummary ?? {}) };
  rollbackBundles(layout);
  const current = await control(home, "status");
  const staleStartedAt = new Set([
    before.started_at_unix_ms,
    first.status?.outcome === "status" ? first.status.started_at_unix_ms : null,
    current?.outcome === "status" ? current.started_at_unix_ms : null,
  ].filter(sequence));
  let second = null;
  if (current?.outcome === "status") {
    const restartAck = await control(home, "restart");
    if (restartAck?.outcome === "status") {
      second = await waitForStatus(home, (status) => healthy(status) && sequence(status.started_at_unix_ms)
        && !staleStartedAt.has(status.started_at_unix_ms), timeoutMs, statusPoll);
      if (second.ok) {
        return { ok: false, applied: true, swapped: swap, rolled_back: true, recovered: true, new_interface_failures: regressed,
          failed: first.status, runtime: second.status, ...(waitSummary ?? {}) };
      }
      if (second.status?.outcome === "status" && sequence(second.status.started_at_unix_ms)) {
        staleStartedAt.add(second.status.started_at_unix_ms);
      }
    }
  }
  const recovered = await recoverRolledBackRuntime(layout, home, { before, first, current, second, staleStartedAt, healthy, options });
  return {
    ok: false, applied: true, swapped: swap, rolled_back: true, recovered: recovered.recovered,
    new_interface_failures: regressed, failed: first.status, runtime: recovered.runtime,
    ...(recovered.error ? { error: recovered.error } : {}),
    recovery: recovered.recovery,
    ...(waitSummary ?? {}),
  };
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
  verify [--no-smoke] [--allow-modified-source]
  packages status | packages sync [--dry-run] [--with-chat-client] [--only=a,b]
  swap [--dry-run]                Octopus.app -> Octopus.prev.app, staging -> Octopus.app
  restart [--swap] [--caller-run=<run id>] [--allow-in-flight-run <run id>]... [--wait-quiet[=<seconds>]] [--quiet-for=<seconds>] [--dry-run] [--force] [--no-rollback] [--detach --log=<file>]
                                  refuses on untrusted turn evidence, a changing frontier, or a blocking turn
  rollback [--dry-run]            Octopus.prev.app -> Octopus.app
Common: --home=<Octopus Home, default $OCTOPUS_HOME or ~/.octopus> --build-root=<dir>`;

const REPEATABLE_VALUE_OPTIONS = new Set(["allow-in-flight-run"]);

function parseSecondsOption(raw, name, { min, max }) {
  if (typeof raw !== "string" || !raw.trim()) throw Object.assign(new Error(`--${name} requires seconds between ${min} and ${max}`), { usage: true });
  const seconds = Number(raw);
  if (!Number.isFinite(seconds) || seconds < min || seconds > max) {
    throw Object.assign(new Error(`--${name} must be between ${min} and ${max} seconds`), { usage: true });
  }
  return Math.round(seconds * 1000);
}

function resolveQuietWaitOptions(options) {
  let waitQuietMs = 0;
  if (options.waitQuiet === true) waitQuietMs = DEFAULT_WAIT_QUIET_MS;
  else if (options.waitQuiet !== undefined) waitQuietMs = parseSecondsOption(options.waitQuiet, "wait-quiet", { min: 1, max: 3600 });
  let quietWindowMs = waitQuietMs > 0 ? DEFAULT_QUIET_WINDOW_MS : 0;
  if (options.quietFor !== undefined) {
    if (waitQuietMs === 0) throw Object.assign(new Error("--quiet-for requires --wait-quiet"), { usage: true });
    quietWindowMs = parseSecondsOption(options.quietFor, "quiet-for", { min: 0, max: 300 });
  }
  return { waitQuietMs, quietWindowMs };
}

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
    options[key] = rest.length ? rest.join("=") : true;
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
    const { waitQuietMs, quietWindowMs } = resolveQuietWaitOptions(options);
    if (options.detach === true && !dryRun) {
      if (typeof options.log !== "string") throw new Error("--detach requires --log=<file>");
      const log = fs.openSync(path.resolve(options.log), "a");
      try {
        const child = spawn(process.execPath, [__filename, ...argv.filter((arg) => arg !== "--detach")], { detached: true, stdio: ["ignore", log, log] });
        child.unref();
        return { detached: true, pid: child.pid, log: path.resolve(options.log) };
      } finally {
        fs.closeSync(log);
      }
    }
    return restartRuntime(layout, home, { callerRun: typeof options.callerRun === "string" ? options.callerRun : null, dryRun,
      allowInFlightRuns: Array.isArray(options.allowInFlightRun) ? options.allowInFlightRun : [],
      force: options.force === true, swap: options.swap === true, rollback: options.noRollback !== true,
      waitQuietMs, quietWindowMs });
  }
  throw Object.assign(new Error(USAGE), { usage: true });
}

module.exports = { bundles, buildInfo, swapBundles, rollbackBundles, control, isReady, readChatTurns, readLedgerTurns, readTurnsInFlight, blockingTurns,
  newInterfaceFailures, packageAction, restartRuntime, parseArgs, resolveQuietWaitOptions,
  readRuntimeInstanceState, parseLaunchdService, inspectLaunchdService, terminateProcess, runtimeEnv, main };

if (require.main === module) {
  main(process.argv.slice(2)).then((result) => {
    console.log(JSON.stringify(result, null, 2));
    process.exitCode = result?.ok === false ? 1 : 0;
  }, (error) => { console.error(error.usage ? error.message : `safe-self-update: ${error.message}`); process.exitCode = 2; });
}
