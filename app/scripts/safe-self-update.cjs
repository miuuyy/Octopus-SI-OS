#!/usr/bin/env node
const crypto = require("node:crypto");
const fs = require("node:fs");
const net = require("node:net");
const os = require("node:os");
const path = require("node:path");
const { spawn, spawnSync } = require("node:child_process");
const { setTimeout: delay } = require("node:timers/promises");

const appRoot = path.resolve(__dirname, "..");
const repoRoot = path.resolve(appRoot, "..");

const VALID_PLATFORMS = new Set(["darwin", "linux"]);
const VALID_TARGETS = new Set(["app", "cli"]);
const VALID_LAYOUTS = new Set(["app", "cli", "macos-app", "linux-cli"]);

function resolveLayoutOptions(options = {}) {
  const platform = options.platform !== undefined ? options.platform : undefined;
  const target = options.target !== undefined ? options.target : undefined;
  const layout = options.layout !== undefined ? options.layout : undefined;
  const installDir = options.installDir !== undefined ? options.installDir : undefined;
  if (platform !== undefined && (typeof platform !== "string" || !VALID_PLATFORMS.has(platform))) {
    throw Object.assign(new Error(`--platform must be one of: ${[...VALID_PLATFORMS].join(", ")}`), { usage: true });
  }
  if (target !== undefined && (typeof target !== "string" || !VALID_TARGETS.has(target))) {
    throw Object.assign(new Error(`--target must be one of: ${[...VALID_TARGETS].join(", ")}`), { usage: true });
  }
  if (layout !== undefined && (typeof layout !== "string" || !VALID_LAYOUTS.has(layout))) {
    throw Object.assign(new Error(`--layout must be one of: ${[...VALID_LAYOUTS].join(", ")}`), { usage: true });
  }
  if (installDir !== undefined && (typeof installDir !== "string" || !installDir.trim())) {
    throw Object.assign(new Error("--install-dir requires a directory path"), { usage: true });
  }
  const wantsCli = target === "cli" || layout === "cli" || layout === "linux-cli";
  const wantsApp = target === "app" || layout === "app" || layout === "macos-app";
  if (wantsCli && wantsApp) {
    throw Object.assign(new Error("--target and --layout conflict between app and cli"), { usage: true });
  }
  if (installDir && wantsApp) {
    throw Object.assign(new Error("--install-dir is supported only for the Linux CLI layout"), { usage: true });
  }
  const kind = (installDir || wantsCli || (!wantsApp && platform === "linux")) ? "linux-cli" : "macos-app";
  return { kind, platform: platform ?? (kind === "linux-cli" ? "linux" : "darwin"), installDir: installDir ? path.resolve(installDir) : null };
}

function bundles(buildRoot = path.join(appRoot, "build"), options = {}) {
  const resolved = resolveLayoutOptions(options);
  if (resolved.kind === "linux-cli") {
    if (resolved.installDir) {
      const live = resolved.installDir;
      const parent = path.dirname(live);
      const base = path.basename(live);
      if (!base || live === path.parse(live).root) {
        throw Object.assign(new Error(`--install-dir cannot be a filesystem root: ${live}`), { usage: true });
      }
      const atParent = (suffix) => path.join(parent, `${base}${suffix}`);
      return { kind: "linux-cli", buildRoot: parent, installDir: live, live, staging: atParent(".staging"), prev: atParent(".prev"), failed: atParent(".failed") };
    }
    const atCli = (name) => path.join(buildRoot, name);
    return { kind: "linux-cli", buildRoot, installDir: atCli("cli"), live: atCli("cli"), staging: atCli("cli.staging"), prev: atCli("cli.prev"), failed: atCli("cli.failed") };
  }
  const at = (name) => path.join(buildRoot, name);
  return { kind: "macos-app", buildRoot, live: at("Octopus.app"), staging: at("Octopus.staging.app"), prev: at("Octopus.prev.app"), failed: at("Octopus.failed.app") };
}

const isCliLayout = (layout) => layout?.kind === "linux-cli";

function isStandaloneCliTarget(layout) {
  if (!isCliLayout(layout)) return false;
  if (fs.existsSync(layout.live)) {
    return resolveCliBundlePaths(layout.live).mode === "standalone";
  }
  return Boolean(layout.installDir)
    && normalizeFsPath(layout.live) !== normalizeFsPath(path.join(appRoot, "build", "cli"));
}

function resolveCliBundlePaths(bundle) {
  const standaloneCli = path.join(bundle, "app", "build", "cli");
  const standaloneReceipt = path.join(standaloneCli, "runtime", "runtime-build-receipt.json");
  const standaloneShared = path.join(standaloneCli, "shared.cjs");
  if (fs.existsSync(standaloneReceipt) || fs.existsSync(standaloneShared) || fs.existsSync(standaloneCli)) {
    return {
      mode: "standalone",
      cliDir: standaloneCli,
      runtimeRoot: path.join(standaloneCli, "runtime"),
      electronDir: path.join(bundle, "app", "electron"),
      launcher: path.join(bundle, "octopus"),
      sharedEntry: standaloneShared,
    };
  }
  return {
    mode: "checkout",
    cliDir: bundle,
    runtimeRoot: path.join(bundle, "runtime"),
    electronDir: fs.existsSync(path.join(bundle, "electron")) ? path.join(bundle, "electron") : path.join(appRoot, "electron"),
    launcher: fs.existsSync(path.join(bundle, "octopus")) ? path.join(bundle, "octopus") : path.join(repoRoot, "octopus"),
    sharedEntry: path.join(bundle, "shared.cjs"),
  };
}

function cliReceipt(bundle) {
  try {
    const receiptPath = path.join(resolveCliBundlePaths(bundle).runtimeRoot, "runtime-build-receipt.json");
    const parsed = JSON.parse(fs.readFileSync(receiptPath, "utf8"));
    if (!parsed || typeof parsed !== "object" || parsed.schema !== "octopus.cli_runtime_distribution") return null;
    return parsed;
  } catch {
    return null;
  }
}

function isCliBundle(bundle, layout = null) {
  if (isCliLayout(layout)) return true;
  if (fs.existsSync(path.join(bundle, "Contents", "Resources", "app", "build-info.json"))) return false;
  return cliReceipt(bundle) !== null;
}

const installation = (bundle, layout = null) => (
  isCliBundle(bundle, layout)
    ? resolveCliBundlePaths(bundle).runtimeRoot
    : path.join(bundle, "Contents", "Resources", "app")
);
const electronRoot = (bundle, layout = null) => (
  isCliBundle(bundle, layout)
    ? resolveCliBundlePaths(bundle).electronDir
    : path.join(installation(bundle, layout), "electron")
);
const runtimeBinary = (bundle, layout = null) => path.join(installation(bundle, layout), "bin", "octopus");

function buildInfo(bundle, layout = null) {
  if (!isCliLayout(layout)) {
    try {
      const macInfo = JSON.parse(fs.readFileSync(path.join(bundle, "Contents", "Resources", "app", "build-info.json"), "utf8"));
      if (macInfo && typeof macInfo === "object") return macInfo;
    } catch {}
  }
  const receipt = cliReceipt(bundle);
  if (!receipt) return null;
  return {
    schema: receipt.schema,
    schema_version: receipt.schema_version ?? 1,
    kind: "linux-cli",
    package_version: receipt.package_version ?? null,
    platform: receipt.platform ?? null,
    architecture: receipt.architecture ?? null,
    target: receipt.target ?? null,
    revision: receipt.revision ?? receipt.package_version ?? null,
    modifiedSource: typeof receipt.modifiedSource === "boolean" ? receipt.modifiedSource : false,
    built_at_unix_ms: receipt.built_at_unix_ms ?? null,
    binaries_count: Array.isArray(receipt.binaries) ? receipt.binaries.length : 0,
  };
}

function liveBundleIdentity(layout) {
  const marker = isCliLayout(layout)
    ? path.join(resolveCliBundlePaths(layout.live).runtimeRoot, "runtime-build-receipt.json")
    : path.join(installation(layout.live, layout), "build-info.json");
  const stat = fs.statSync(marker, { throwIfNoEntry: false });
  return stat ? `${stat.ino}:${stat.mtimeMs}:${stat.size}` : null;
}

function requireBundle(bundle, layout = null) {
  if (!fs.statSync(bundle, { throwIfNoEntry: false })?.isDirectory() || !buildInfo(bundle, layout)) {
    throw new Error(`${bundle} is not a packaged Octopus bundle`);
  }
  return bundle;
}

function swapBundles(layout, { dryRun = false } = {}) {
  requireBundle(layout.staging, layout);
  const hadLive = fs.existsSync(layout.live);
  const plan = [`remove ${layout.prev}`, ...(hadLive ? [`rename ${layout.live} -> ${layout.prev}`] : []), `rename ${layout.staging} -> ${layout.live}`];
  if (dryRun) return { applied: false, plan };
  if (hadLive) {
    requireBundle(layout.live, layout);
    fs.rmSync(layout.prev, { recursive: true, force: true });
    fs.renameSync(layout.live, layout.prev);
  }
  try { fs.renameSync(layout.staging, layout.live); }
  catch (error) { if (hadLive) fs.renameSync(layout.prev, layout.live); throw error; }
  return { applied: true, plan, revision: buildInfo(layout.live, layout).revision };
}

function rollbackBundles(layout, { dryRun = false } = {}) {
  requireBundle(layout.prev, layout);
  const hadLive = fs.existsSync(layout.live);
  const plan = [`remove ${layout.failed}`, ...(hadLive ? [`rename ${layout.live} -> ${layout.failed}`] : []), `rename ${layout.prev} -> ${layout.live}`];
  if (dryRun) return { applied: false, plan };
  fs.rmSync(layout.failed, { recursive: true, force: true });
  if (hadLive) fs.renameSync(layout.live, layout.failed);
  try { fs.renameSync(layout.prev, layout.live); }
  catch (error) { if (hadLive) fs.renameSync(layout.failed, layout.live); throw error; }
  return { applied: true, plan, revision: buildInfo(layout.live, layout).revision };
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

async function waitForStatus(home, accept, timeoutMs, pollMs = 250, { abortCheck = null } = {}) {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    const status = await control(home, "status", 2000);
    if (accept(status)) return { ok: true, status };
    if (typeof abortCheck === "function") {
      const abort = await abortCheck(status);
      if (abort) return { ok: false, aborted: true, abort, status };
    }
    if (Date.now() >= deadline) return { ok: false, status };
    await delay(pollMs);
  }
}

const DEFAULT_LAUNCHD_SERVICE_LABEL = "app.octopus.runtime";
const DEFAULT_SYSTEMD_SERVICE = "octopus.service";
const DEFAULT_SYSTEMD_AUTHORITY_SERVICE = "octopus-process-authority.service";
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

function defaultSystemctl(args, { userService = false } = {}) {
  const fullArgs = userService ? ["--user", ...args] : args;
  const result = spawnSync("systemctl", fullArgs, { encoding: "utf8", timeout: 15_000 });
  return {
    status: result.status,
    stdout: result.stdout ?? "",
    stderr: result.stderr ?? "",
    error: result.error ? result.error.message : null,
  };
}

function tokenizeShellWords(text) {
  const tokens = [];
  let current = "";
  let quote = null;
  for (let i = 0; i < text.length; i += 1) {
    const ch = text[i];
    if (quote) {
      if (ch === "\\" && quote === "\"" && i + 1 < text.length) { current += text[++i]; continue; }
      if (ch === quote) { quote = null; continue; }
      current += ch;
      continue;
    }
    if (ch === "\"" || ch === "'") { quote = ch; continue; }
    if (/\s/.test(ch)) {
      if (current.length) { tokens.push(current); current = ""; }
      continue;
    }
    if (ch === "\\" && i + 1 < text.length) { current += text[++i]; continue; }
    current += ch;
  }
  if (current.length) tokens.push(current);
  return tokens;
}

function parseSystemctlExecStart(raw) {
  const text = typeof raw === "string" ? raw.trim() : "";
  if (!text) return { path: null, argv: [], homeArg: null, bundleArg: null };
  let execPath = null;
  let argv = [];
  if (text.startsWith("{")) {
    const pathMatch = /\bpath=([^;]+?)(?:\s*;|\s*})/.exec(text);
    const argvMatch = /\bargv\[\]=([^;]+?)(?:\s*;|\s*})/.exec(text);
    if (pathMatch) execPath = pathMatch[1].trim();
    if (argvMatch) argv = tokenizeShellWords(argvMatch[1].trim());
    if (!execPath && argv.length) execPath = argv[0];
  } else {
    argv = tokenizeShellWords(text);
    execPath = argv[0] ?? null;
  }
  let homeArg = null;
  let bundleArg = null;
  for (let i = 1; i < argv.length; i += 1) {
    const arg = argv[i];
    if (arg === "--home" || arg === "--state-dir") {
      if (i + 1 < argv.length) { homeArg = argv[++i]; }
    } else if (arg.startsWith("--home=")) {
      homeArg = arg.slice("--home=".length);
    } else if (arg.startsWith("--state-dir=")) {
      homeArg = arg.slice("--state-dir=".length);
    } else if (arg === "--bundle") {
      if (i + 1 < argv.length) { bundleArg = argv[++i]; }
    } else if (arg.startsWith("--bundle=")) {
      bundleArg = arg.slice("--bundle=".length);
    }
  }
  return { path: execPath, argv, homeArg, bundleArg };
}

function parseSystemctlEnvironment(raw) {
  const env = {};
  if (typeof raw !== "string" || !raw.trim()) return env;
  for (const token of tokenizeShellWords(raw)) {
    const eq = token.indexOf("=");
    if (eq > 0) env[token.slice(0, eq)] = token.slice(eq + 1);
  }
  return env;
}

function parseSystemctlShow(stdout) {
  const props = {};
  for (const line of (typeof stdout === "string" ? stdout : "").split(/\r?\n/)) {
    const eq = line.indexOf("=");
    if (eq <= 0) continue;
    props[line.slice(0, eq).trim()] = line.slice(eq + 1).trim();
  }
  const mainPidNum = props.MainPID ? Number(props.MainPID) : 0;
  const nRestartsNum = props.NRestarts !== undefined && props.NRestarts !== "" ? Number(props.NRestarts) : null;
  const execMainStatusNum = props.ExecMainStatus !== undefined && props.ExecMainStatus !== "" ? Number(props.ExecMainStatus) : null;
  const execInfo = parseSystemctlExecStart(props.ExecStart);
  const envMap = parseSystemctlEnvironment(props.Environment);
  return {
    loadState: props.LoadState || null,
    activeState: props.ActiveState || null,
    subState: props.SubState || null,
    unitFileState: props.UnitFileState || null,
    mainPid: Number.isSafeInteger(mainPidNum) && mainPidNum > 0 ? mainPidNum : null,
    nRestarts: Number.isSafeInteger(nRestartsNum) && nRestartsNum >= 0 ? nRestartsNum : null,
    result: props.Result || null,
    execMainStatus: Number.isSafeInteger(execMainStatusNum) ? execMainStatusNum : null,
    execStart: execInfo.path,
    argv: execInfo.argv,
    homeArg: execInfo.homeArg,
    bundleArg: execInfo.bundleArg,
    environment: envMap,
    octopusHome: execInfo.homeArg || envMap.OCTOPUS_HOME || null,
    workingDirectory: props.WorkingDirectory || null,
    fragmentPath: props.FragmentPath || null,
  };
}

function inspectSystemdService(layout, home, {
  platform = process.platform,
  serviceName = DEFAULT_SYSTEMD_SERVICE,
  authorityServiceName = DEFAULT_SYSTEMD_AUTHORITY_SERVICE,
  userService = false,
  systemctl = defaultSystemctl,
} = {}) {
  if (platform !== "linux" && !isCliLayout(layout)) {
    return { loaded: false, matchesTarget: false, service: serviceName, userService, reason: "non_linux" };
  }
  if (!nonempty(serviceName)) {
    return { loaded: false, matchesTarget: false, service: null, userService, reason: "invalid_service_name" };
  }
  const props = "LoadState,ActiveState,SubState,UnitFileState,MainPID,NRestarts,Result,ExecMainStatus,ExecStart,Environment,WorkingDirectory,FragmentPath";
  const result = systemctl(["show", serviceName, "--no-pager", `--property=${props}`], { userService });
  if (!result || result.status !== 0) {
    return {
      loaded: false,
      matchesTarget: false,
      service: serviceName,
      userService,
      reason: result?.error ? `systemctl_error: ${result.error}` : (lastLines(result ?? {}) || "systemctl_failed"),
    };
  }
  const parsed = parseSystemctlShow(result.stdout);
  let authorityService = null;
  if (nonempty(authorityServiceName)) {
    const authRes = systemctl(["show", authorityServiceName, "--no-pager", "--property=LoadState,ActiveState,SubState,MainPID"], { userService: false });
    if (authRes && authRes.status === 0) {
      const authParsed = parseSystemctlShow(authRes.stdout);
      authorityService = {
        service: authorityServiceName,
        loaded: authParsed.loadState === "loaded",
        loadState: authParsed.loadState,
        activeState: authParsed.activeState,
        subState: authParsed.subState,
        pid: authParsed.mainPid,
      };
    } else {
      authorityService = {
        service: authorityServiceName,
        loaded: false,
        activeState: "unknown",
        reason: authRes?.error || lastLines(authRes ?? {}) || "systemctl_failed",
      };
    }
  }
  if (parsed.loadState !== "loaded") {
    return {
      loaded: false,
      matchesTarget: false,
      service: serviceName,
      userService,
      loadState: parsed.loadState,
      activeState: parsed.activeState,
      subState: parsed.subState,
      unitFileState: parsed.unitFileState,
      authorityService,
      reason: parsed.loadState === "not-found" ? "not_found" : "not_loaded",
    };
  }
  const expectedHome = normalizeFsPath(home);
  if (parsed.octopusHome && normalizeFsPath(parsed.octopusHome) !== expectedHome) {
    return {
      loaded: true,
      matchesTarget: false,
      service: serviceName,
      userService,
      ...parsed,
      pid: parsed.mainPid,
      authorityService,
      reason: `systemd service OCTOPUS_HOME (${parsed.octopusHome}) does not match ${home}`,
    };
  }
  const expectedLive = normalizeFsPath(layout.live);
  const cliPaths = resolveCliBundlePaths(layout.live);
  const expectedRuntimeRoot = normalizeFsPath(cliPaths.runtimeRoot);
  const expectedRuntimeBin = normalizeFsPath(runtimeBinary(layout.live, layout));
  const expectedLauncher = normalizeFsPath(cliPaths.launcher);
  const expectedRepoLauncher = normalizeFsPath(path.join(repoRoot, "octopus"));
  const isCheckoutLive = normalizeFsPath(layout.live) === normalizeFsPath(path.join(appRoot, "build", "cli"));
  if (parsed.bundleArg) {
    const normBundle = normalizeFsPath(parsed.bundleArg);
    const bundleOk = normBundle === expectedLive || normBundle === expectedRuntimeRoot
      || normBundle.startsWith(`${expectedLive}${path.sep}`);
    if (!bundleOk) {
      return {
        loaded: true,
        matchesTarget: false,
        service: serviceName,
        userService,
        ...parsed,
        pid: parsed.mainPid,
        authorityService,
        reason: `systemd service --bundle (${parsed.bundleArg}) does not match ${layout.live}`,
      };
    }
  } else if (parsed.execStart) {
    const normExec = normalizeFsPath(parsed.execStart);
    const execOk = normExec === expectedRuntimeBin || normExec === expectedLauncher
      || normExec.startsWith(`${expectedLive}${path.sep}`)
      || (isCheckoutLive && normExec === expectedRepoLauncher);
    if (!execOk) {
      return {
        loaded: true,
        matchesTarget: false,
        service: serviceName,
        userService,
        ...parsed,
        pid: parsed.mainPid,
        authorityService,
        reason: `systemd service ExecStart (${parsed.execStart}) does not target ${layout.live}`,
      };
    }
  }
  return {
    loaded: true,
    matchesTarget: true,
    service: serviceName,
    userService,
    ...parsed,
    pid: parsed.mainPid,
    authorityService,
    reason: null,
  };
}

const isPermissionError = (text) => /interactive authentication required|access denied|permission denied|polkit|authentication is required|must be root|operation not permitted/i.test(text || "");

function isSystemdUnitFailedOrCrashLooping(unit) {
  if (!unit || (!unit.loaded && unit.loadState !== "loaded")) return null;
  const label = unit.service || "systemd unit";
  if (unit.activeState === "failed" || unit.subState === "failed") {
    return `systemd unit ${label} entered failed state (${unit.activeState}/${unit.subState}${unit.result ? `, result=${unit.result}` : ""})`;
  }
  if (unit.subState === "auto-restart") {
    return `systemd unit ${label} entered crash-loop (${unit.activeState}/${unit.subState}${Number.isSafeInteger(unit.nRestarts) ? `, restarts=${unit.nRestarts}` : ""})`;
  }
  if (Number.isSafeInteger(unit.nRestarts) && unit.nRestarts >= 1) {
    return `systemd unit ${label} restarted unexpectedly (NRestarts=${unit.nRestarts}${unit.result ? `, result=${unit.result}` : ""})`;
  }
  return null;
}

function restartSystemdService(serviceName, { userService = false, systemctl = defaultSystemctl } = {}) {
  const result = systemctl(["restart", serviceName], { userService });
  if (result && result.status === 0) {
    return { ok: true, service: serviceName, userService, permissionDenied: false };
  }
  const detail = result?.error || lastLines(result ?? {}) || `exit status ${result?.status}`;
  const permissionDenied = isPermissionError(detail);
  return {
    ok: false,
    service: serviceName,
    userService,
    status: result?.status ?? null,
    permissionDenied,
    error: permissionDenied
      ? `systemctl restart ${serviceName} refused due to missing privileges (safe-self-update never invokes sudo automatically): ${detail}`
      : `systemctl restart ${serviceName} failed: ${detail}`,
  };
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

function runtimeEnv(bundle, home, layout = null) {
  const root = installation(bundle, layout), user = os.userInfo().username;
  const codexHome = process.env.CODEX_HOME || path.join(os.homedir(), ".codex");
  return {
    HOME: os.homedir(), USER: user, LOGNAME: user, TMPDIR: os.tmpdir(),
    CODEX_HOME: codexHome,
    OCTOPUS_MACHINE_CODEX_AUTH_FILE: path.join(codexHome, "auth.json"),
    OCTOPUS_MACHINE_CODEX_AUTH_LOCK_FILE: path.join(codexHome, "auth.lock"),
    PATH: [path.join(root, "bin"), "/usr/local/bin", "/usr/bin", "/bin", "/usr/sbin", "/sbin"].join(":"),
    OCTOPUS_HOME: home, OCTOPUS_INSTALLATION_ROOT: root,
    OCTOPUS_SOURCE_REGISTRY: path.join(root, "registry"), OCTOPUS_RUNTIME_BIN: runtimeBinary(bundle, layout),
  };
}

function octopus(bundle, home, args, options = {}, layout = null) {
  return spawnSync(runtimeBinary(bundle, layout), args, { env: runtimeEnv(bundle, home, layout), encoding: "utf8", maxBuffer: 64 * 1024 * 1024, ...options });
}

async function readClientTurns(bundle, home, layout = null) {
  const root = installation(bundle, layout);
  const electronDir = electronRoot(bundle, layout);
  const { createProjectionStore } = require(path.join(electronDir, "projection.cjs"));
  const { projectionReadRequest } = require(path.join(electronDir, "projectionReadContract.cjs"));
  const store = createProjectionStore({
    resolveRepoRoot: () => repoRoot,
    sourceRegistry: () => path.join(root, "registry"),
    localPackageRegistry: () => path.join(root, "local-packages", "registry"),
    localPackageLibrary: () => path.join(root, "local-packages"),
    octopusHome: () => home,
    projectEePolicies: async () => JSON.parse(octopus(bundle, home, ["project-ee-policies"], {}, layout).stdout),
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

const readChatTurns = async (bundle, home, layout = null) => (await readClientTurns(bundle, home, layout)).chats;

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

async function readTurnsInFlight(bundle, home, { sinceUnixMs = 0, layout = null } = {}) {
  const { chats, clientRunIds } = await readClientTurns(bundle, home, layout);
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

const GIB = 1024 ** 3;
const DEFAULT_MIN_FREE_GB = 15;
const roundGb = (bytes) => Math.round((bytes / GIB) * 100) / 100;

function resolveMinFreeGb(rawFlag, envValue = process.env.OCTOPUS_SELF_UPDATE_MIN_FREE_GB) {
  const chosen = rawFlag !== undefined ? rawFlag : envValue;
  if (chosen === undefined || chosen === null || chosen === "") return DEFAULT_MIN_FREE_GB;
  if (typeof chosen === "boolean") {
    throw Object.assign(new Error("--min-free-gb requires a positive number of GiB"), { usage: true });
  }
  const parsed = Number(chosen);
  if (!Number.isFinite(parsed) || parsed <= 0 || parsed > 10_000) {
    throw Object.assign(new Error("--min-free-gb / OCTOPUS_SELF_UPDATE_MIN_FREE_GB must be a positive number up to 10000 GiB"), { usage: true });
  }
  return parsed;
}

function existingAncestor(targetPath) {
  let cursor = path.resolve(targetPath);
  for (;;) {
    if (fs.existsSync(cursor)) return cursor;
    const parent = path.dirname(cursor);
    if (parent === cursor) return cursor;
    cursor = parent;
  }
}

function checkBuildDiskSpace(targetPath, { minFreeGb = DEFAULT_MIN_FREE_GB, statfs = fs.statfsSync } = {}) {
  const requiredGb = resolveMinFreeGb(minFreeGb, null);
  const probePath = existingAncestor(targetPath);
  const stats = statfs(probePath);
  const availableBytes = Number(stats?.bavail) * Number(stats?.bsize);
  if (!Number.isFinite(availableBytes) || availableBytes < 0) {
    throw new Error(`unable to determine available disk space for ${probePath}`);
  }
  const requiredBytes = Math.ceil(requiredGb * GIB);
  return {
    ok: availableBytes >= requiredBytes,
    path: probePath,
    available_bytes: availableBytes,
    available_gb: roundGb(availableBytes),
    required_bytes: requiredBytes,
    required_gb: roundGb(requiredBytes),
  };
}

function buildStaging(layout, { dryRun = false, minFreeGb = undefined, cliTarget = null, statfs = fs.statfsSync, spawn = spawnSync, populateStandalone = null } = {}) {
  const disk = checkBuildDiskSpace(layout.buildRoot, { minFreeGb: resolveMinFreeGb(minFreeGb), statfs });
  let env;
  let command;
  const standaloneCli = isStandaloneCliTarget(layout);
  if (isCliLayout(layout)) {
    const cliBuildDir = standaloneCli ? path.join(layout.staging, "app", "build", "cli") : layout.staging;
    env = { ...process.env, OCTOPUS_CLI_BUILD_DIR: cliBuildDir };
    command = [process.execPath, path.join(__dirname, "build-cli.cjs"), "--runtime", ...(nonempty(cliTarget) ? [`--target=${cliTarget}`] : [])];
  } else {
    const { node } = JSON.parse(fs.readFileSync(path.join(appRoot, "native", "runtime.json"), "utf8"));
    env = { ...process.env, OCTOPUS_PACKAGE_BUNDLE: layout.staging,
      OCTOPUS_NATIVE_NODE_HEADERS: process.env.OCTOPUS_NATIVE_NODE_HEADERS || path.join(layout.buildRoot, `node-v${node}`, "include", "node") };
    command = [process.execPath, path.join(__dirname, "package-macos.cjs")];
  }
  if (dryRun) return { ok: disk.ok, applied: false, command, bundle: layout.staging, ...(isCliLayout(layout) ? { standalone: standaloneCli } : {}), disk };
  if (!disk.ok) {
    throw new Error(
      `insufficient free disk space on ${disk.path}: ${disk.available_gb} GiB available, ${disk.required_gb} GiB required; `
      + "run 'node app/scripts/safe-self-update.cjs prune' to reclaim rebuildable artifacts or adjust --min-free-gb",
    );
  }
  const before = liveBundleIdentity(layout);
  if (standaloneCli) {
    fs.rmSync(layout.staging, { recursive: true, force: true });
    fs.mkdirSync(path.join(layout.staging, "app", "build", "cli"), { recursive: true });
  }
  const result = spawn(command[0], command.slice(1), { cwd: repoRoot, env, stdio: ["ignore", 2, 2] });
  if (result.status !== 0) throw new Error(`packaging failed with status ${result.status}; ${layout.live} was not touched`);
  if (standaloneCli) {
    const populate = populateStandalone ?? require("./release-linux.cjs").populateStandaloneLinuxDir;
    populate(layout.staging, { appRoot, repoRoot, copyCliRuntime: false });
  }
  if (liveBundleIdentity(layout) !== before) throw new Error(`${layout.live} changed during the staging build`);
  const staged = buildInfo(layout.staging, layout);
  if (!staged) throw new Error(`staging build finished without a readable Octopus bundle at ${layout.staging}`);
  return { applied: true, bundle: layout.staging, ...(isCliLayout(layout) ? { standalone: standaloneCli } : {}), build: staged, live_untouched: true, disk };
}

function pathOverlaps(candidate, protectedPath) {
  const a = normalizeFsPath(candidate);
  const b = normalizeFsPath(protectedPath);
  return a === b || a.startsWith(`${b}${path.sep}`) || b.startsWith(`${a}${path.sep}`);
}

function entryBytes(rootPath) {
  const seenInodes = new Set();
  let total = 0;
  const walk = (current) => {
    const stat = fs.lstatSync(current);
    if (stat.isSymbolicLink()) return;
    if (stat.isDirectory()) {
      for (const name of fs.readdirSync(current)) walk(path.join(current, name));
      return;
    }
    if (stat.isFile()) {
      const key = `${stat.dev}:${stat.ino}`;
      if (!seenInodes.has(key)) {
        seenInodes.add(key);
        total += stat.size;
      }
    }
  };
  walk(rootPath);
  return total;
}

function findProtectedOverlap(resolvedPath, protectedRoots) {
  const normalized = normalizeFsPath(resolvedPath);
  for (const guard of protectedRoots) {
    const guardNorm = normalizeFsPath(guard.path);
    const isStrictChildOfRepoOrBuild = (guard.label === "repo_root" || guard.label === "build_root")
      && normalized.startsWith(`${guardNorm}${path.sep}`);
    if (isStrictChildOfRepoOrBuild) continue;
    if (pathOverlaps(normalized, guardNorm)) {
      return guard;
    }
  }
  return null;
}

function hasCargoWorkspace(rootDir) {
  return Boolean(fs.statSync(path.join(rootDir, "Cargo.toml"), { throwIfNoEntry: false })?.isFile());
}

function pruneBuildArtifacts(layout, home, {
  dryRun = false,
  includePrev = false,
  includeCli = false,
  targetDir = undefined,
  sourceRoot = repoRoot,
} = {}) {
  const effectiveSourceRoot = path.resolve(sourceRoot);
  const protectedRoots = [
    { label: "live_bundle", path: layout.live },
    { label: "octopus_home", path: home },
    { label: "repo_root", path: effectiveSourceRoot },
    { label: "build_root", path: layout.buildRoot },
  ];
  const skipped = [];
  if (includeCli && isCliLayout(layout)) {
    skipped.push({ label: "cli_build", path: path.resolve(layout.live), reason: "protected_live_bundle" });
  }
  const hasExplicitTargetDir = targetDir !== undefined;
  let rustTargetCandidate = null;
  if (hasExplicitTargetDir) {
    if (targetDir) {
      rustTargetCandidate = { label: "rust_target", path: path.resolve(targetDir) };
    }
  } else {
    const defaultTarget = path.resolve(effectiveSourceRoot, "target");
    const overlap = findProtectedOverlap(defaultTarget, protectedRoots);
    if (overlap) {
      skipped.push({ label: "rust_target", path: defaultTarget, reason: `protected_${overlap.label}` });
    } else if (!hasCargoWorkspace(effectiveSourceRoot)) {
      skipped.push({ label: "rust_target", path: defaultTarget, reason: "no_cargo_workspace" });
    } else {
      rustTargetCandidate = { label: "rust_target", path: defaultTarget };
    }
  }
  const targets = [
    { label: "staging_bundle", path: layout.staging },
    { label: "failed_bundle", path: layout.failed },
    ...(rustTargetCandidate ? [rustTargetCandidate] : []),
    ...(includePrev ? [{ label: "previous_bundle", path: layout.prev }] : []),
    ...(includeCli && !isCliLayout(layout) ? [{ label: "cli_build", path: path.join(layout.buildRoot, "cli") }] : []),
  ];
  const actionable = [];
  const seenTargets = new Set();
  for (const item of targets) {
    const resolved = path.resolve(item.path);
    if (resolved === path.parse(resolved).root) {
      throw new Error(`refusing to prune filesystem root for ${item.label}: ${resolved}`);
    }
    const normalized = normalizeFsPath(resolved);
    if (seenTargets.has(normalized)) continue;
    seenTargets.add(normalized);
    const overlap = findProtectedOverlap(resolved, protectedRoots);
    if (overlap) {
      throw new Error(`refusing to prune ${item.label} (${resolved}): overlaps protected ${overlap.label} (${overlap.path})`);
    }
    const stat = fs.lstatSync(resolved, { throwIfNoEntry: false });
    if (!stat) {
      skipped.push({ label: item.label, path: resolved, reason: "absent" });
      continue;
    }
    if (stat.isSymbolicLink()) {
      throw new Error(`refusing to prune symlinked ${item.label}: ${resolved}`);
    }
    actionable.push({ label: item.label, path: resolved, bytes: entryBytes(resolved) });
  }
  if (dryRun) {
    const wouldReclaim = actionable.reduce((sum, entry) => sum + entry.bytes, 0);
    return {
      ok: true,
      applied: false,
      dry_run: true,
      would_reclaim_bytes: wouldReclaim,
      reclaimed_bytes: 0,
      candidates: actionable,
      skipped,
    };
  }
  const beforeLive = liveBundleIdentity(layout);
  const removed = [];
  let reclaimedBytes = 0;
  for (const entry of actionable) {
    fs.rmSync(entry.path, { recursive: true, force: false });
    removed.push(entry);
    reclaimedBytes += entry.bytes;
  }
  if (liveBundleIdentity(layout) !== beforeLive) {
    throw new Error(`${layout.live} changed during prune`);
  }
  return {
    ok: true,
    applied: true,
    reclaimed_bytes: reclaimedBytes,
    removed,
    skipped,
    live_untouched: true,
  };
}

function packageInspectionRoots(bundle, home, layout = null) {
  const root = installation(bundle, layout);
  return { sourceRegistryRoot: path.join(root, "local-packages", "registry"), activeRegistryRoot: path.join(home, "registry"),
    libraryRoot: path.join(root, "local-packages"), stateRoot: path.join(home, "client", "package-state") };
}

function packageRoots(bundle, home, layout = null) {
  const roots = packageInspectionRoots(bundle, home, layout), root = installation(bundle, layout);
  const { createPackageStoragePublisher } = require(path.join(electronRoot(bundle, layout), "packageStorage.cjs"));
  return { ...roots, publication: createPackageStoragePublisher(roots.sourceRegistryRoot, root) };
}
const localPackages = (bundle, layout = null) => require(path.join(electronRoot(bundle, layout), "localPackages.cjs"));
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

async function availablePackages(bundle, home) {
  const catalog = await localPackages(bundle).inspectLocalPackageCatalog(packageRoots(bundle, home));
  return { ok: catalog.errors.length === 0, packages: catalog.packages, errors: catalog.errors };
}

async function installPackages(bundle, home, packageIds, { dryRun = false } = {}) {
  if (!packageIds.length) throw new Error("packages install requires at least one package id; use packages available");
  if (packageIds.some((id) => !/^[A-Za-z0-9][A-Za-z0-9_.-]*$/.test(id))) throw new Error("Invalid package id; use packages available");
  const catalog = await availablePackages(bundle, home);
  if (!catalog.ok) throw new Error(`Package catalog is invalid: ${catalog.errors.join("; ")}`);
  const byId = new Map(catalog.packages.map((entry) => [entry.id, entry]));
  const closure = [], visiting = new Set(), visited = new Set();
  const visit = (id) => {
    if (visiting.has(id)) throw new Error(`Package dependency cycle at '${id}'`);
    if (visited.has(id)) return;
    const entry = byId.get(id);
    if (!entry) throw new Error(`Unknown package '${id}'; use packages available to list bundled package ids`);
    if (entry.available === false || entry.error) throw new Error(`Package '${id}' is unavailable: ${entry.error || "invalid source"}`);
    if (entry.release_status === "system") throw new Error(`System package '${id}' cannot be installed independently`);
    visiting.add(id);
    (entry.required_packages ?? []).forEach(visit);
    visiting.delete(id);
    visited.add(id);
    closure.push(entry);
  };
  packageIds.forEach(visit);
  const install = closure.filter((entry) => !entry.installed).map((entry) => entry.id);
  const preserved = closure.filter((entry) => entry.installed).map((entry) => entry.id);
  if (dryRun) return { ok: true, applied: false, install, preserved };
  const result = await localPackages(bundle).installLocalPackages({
    ...packageRoots(bundle, home), operationId: `packages-install-${crypto.randomUUID()}`,
    packageIds: closure.map((entry) => entry.id),
    expectedSourceDigests: Object.fromEntries(closure.map((entry) => [entry.id, entry.source_digest])),
  });
  return { ok: true, applied: true, install, preserved, result };
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
  let results = [];
  if (selected.length) {
    try {
      const outcome = await localPackages(bundle).repairLocalPackages({ ...packageRoots(bundle, home),
        operationId: `self-update-${crypto.randomUUID()}`, packageIds: selected.map(row => row.id),
        expectedRepairs: Object.fromEntries(selected.map(row => [row.id, row.expected])) });
      results = selected.map(row => ({ id: row.id, repaired: true, changed: outcome.changed !== false }));
    } catch (error) { results = selected.map(row => ({ id: row.id, repaired: false, error: error.message })); }
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

function sha256File(filePath) {
  return crypto.createHash("sha256").update(fs.readFileSync(filePath)).digest("hex");
}

function isValidRuntimeUsageOutput(result) {
  if (!result || result.error || result.signal) return false;
  if (result.status === 0) return true;
  if (result.status === 2) {
    const text = `${result.stdout ?? ""}\n${result.stderr ?? ""}`;
    return /\boctopus\b/i.test(text) && /\bup\b/.test(text) && /\bdoctor\b/.test(text);
  }
  return false;
}

function checkCliLayoutParity(layout, systemd = null) {
  const stagingPaths = resolveCliBundlePaths(layout.staging);
  if (fs.existsSync(layout.live) && buildInfo(layout.live, layout)) {
    const livePaths = resolveCliBundlePaths(layout.live);
    if (livePaths.mode !== stagingPaths.mode) {
      return {
        ok: false,
        detail: `staging layout (${stagingPaths.mode}) does not match live install layout (${livePaths.mode})`,
      };
    }
  }
  if (systemd?.loaded && systemd?.matchesTarget && nonempty(systemd.execStart)) {
    const normLive = normalizeFsPath(layout.live);
    const normExec = normalizeFsPath(systemd.execStart);
    if (normExec.startsWith(`${normLive}${path.sep}`)) {
      const relExec = path.relative(normLive, normExec);
      const stagedExec = path.join(layout.staging, relExec);
      const stat = fs.statSync(stagedExec, { throwIfNoEntry: false });
      if (!stat || !stat.isFile() || !(stat.mode & 0o111)) {
        return {
          ok: false,
          detail: `staged bundle is missing executable ExecStart target ${relExec}`,
        };
      }
    }
  }
  return {
    ok: true,
    detail: `mode=${stagingPaths.mode}${systemd?.loaded && systemd?.matchesTarget && nonempty(systemd.execStart) ? `, ExecStart parity ok` : ""}`,
  };
}

function verifyCliStaging(layout, home, {
  spawn = spawnSync,
  platform = "linux",
  serviceName = DEFAULT_SYSTEMD_SERVICE,
  authorityServiceName = DEFAULT_SYSTEMD_AUTHORITY_SERVICE,
  userService = false,
  systemctl = defaultSystemctl,
} = {}) {
  const bundle = requireBundle(layout.staging, layout), checks = [];
  const check = (name, ok, detail = "") => checks.push({ name, ok, detail });
  const cliPaths = resolveCliBundlePaths(bundle);
  const receipt = cliReceipt(bundle);
  const info = buildInfo(bundle, layout);
  const systemd = inspectSystemdService(layout, home, { platform, serviceName, authorityServiceName, userService, systemctl });
  check("receipt_schema", Boolean(receipt && receipt.schema === "octopus.cli_runtime_distribution" && receipt.schema_version === 1
    && nonempty(receipt.package_version) && Array.isArray(receipt.binaries) && receipt.binaries.length > 0),
    receipt ? `schema=${receipt.schema} v${receipt.schema_version} package=${receipt.package_version} binaries=${receipt.binaries?.length ?? 0}` : "missing receipt");
  check("platform", receipt?.platform === "linux", `receipt platform=${receipt?.platform ?? "unknown"}`);
  check("shared_entry", Boolean(fs.statSync(cliPaths.sharedEntry, { throwIfNoEntry: false })?.isFile()), cliPaths.sharedEntry);
  if (cliPaths.mode === "standalone") {
    const launcherStat = fs.statSync(cliPaths.launcher, { throwIfNoEntry: false });
    const cliIndex = path.join(bundle, "app", "cli", "index.cjs");
    const cliIndexStat = fs.statSync(cliIndex, { throwIfNoEntry: false });
    const launcherOk = Boolean(launcherStat?.isFile() && (launcherStat.mode & 0o111) && cliIndexStat?.isFile());
    check("standalone_launcher", launcherOk, launcherOk
      ? `${cliPaths.launcher} + app/cli/index.cjs`
      : `missing executable standalone launcher (${cliPaths.launcher}) or app/cli/index.cjs`);
  }
  const parity = checkCliLayoutParity(layout, systemd);
  check("layout_parity", parity.ok, parity.detail);
  const permDirs = [...new Set([bundle, cliPaths.cliDir, cliPaths.runtimeRoot, path.join(cliPaths.runtimeRoot, "bin")])]
    .filter((dir) => fs.existsSync(dir));
  const badDirs = permDirs.filter((dir) => (fs.statSync(dir).mode & 0o777) !== 0o755)
    .map((dir) => `${dir} (0${(fs.statSync(dir).mode & 0o777).toString(8)})`);
  check("directory_permissions", badDirs.length === 0, badDirs.length ? `expected 0755: ${badDirs.join(", ")}` : `${permDirs.length} directories 0755`);
  const binaryErrors = [];
  const seenPaths = new Set();
  for (const entry of Array.isArray(receipt?.binaries) ? receipt.binaries : []) {
    if (!nonempty(entry?.path) || !nonempty(entry?.sha256) || !Number.isSafeInteger(entry?.bytes)) {
      binaryErrors.push("malformed receipt binary entry");
      continue;
    }
    seenPaths.add(entry.path);
    const absPath = path.join(cliPaths.runtimeRoot, entry.path);
    const stat = fs.statSync(absPath, { throwIfNoEntry: false });
    if (!stat || !stat.isFile()) {
      binaryErrors.push(`${entry.path}: missing`);
      continue;
    }
    if (stat.size !== entry.bytes) {
      binaryErrors.push(`${entry.path}: size ${stat.size} != ${entry.bytes}`);
      continue;
    }
    if ((stat.mode & 0o755) !== 0o755) {
      binaryErrors.push(`${entry.path}: mode 0${(stat.mode & 0o777).toString(8)} missing 0755`);
      continue;
    }
    const actualHash = sha256File(absPath);
    if (actualHash !== entry.sha256) {
      binaryErrors.push(`${entry.path}: sha256 mismatch`);
    }
  }
  check("receipt_binaries", binaryErrors.length === 0, binaryErrors.length ? binaryErrors.join("; ") : `${seenPaths.size} binaries verified`);
  const requiredCore = ["bin/octopus", "bin/octopus-linux-process-authority"];
  const missingCore = requiredCore.filter((rel) => !seenPaths.has(rel) || !fs.statSync(path.join(cliPaths.runtimeRoot, rel), { throwIfNoEntry: false })?.isFile());
  check("core_binaries", missingCore.length === 0, missingCore.length ? `missing ${missingCore.join(", ")}` : requiredCore.join(", "));
  const bin = runtimeBinary(bundle, layout);
  const env = runtimeEnv(bundle, home, layout);
  const runtimeHelp = spawn(bin, ["--help"], { env, encoding: "utf8", timeout: 15_000 });
  const runtimeHelpOk = isValidRuntimeUsageOutput(runtimeHelp);
  if (cliPaths.mode === "standalone") {
    const launcherHelp = spawn(cliPaths.launcher, ["--help"], {
      env: { ...env, OCTOPUS_NODE: process.env.OCTOPUS_NODE || process.execPath },
      encoding: "utf8",
      timeout: 15_000,
    });
    const launcherHelpOk = Boolean(launcherHelp && !launcherHelp.error && !launcherHelp.signal && launcherHelp.status === 0);
    const helpDetail = !launcherHelpOk
      ? `standalone launcher --help failed (status=${launcherHelp?.status ?? "err"}): ${launcherHelp?.error ? launcherHelp.error.message : lastLines(launcherHelp ?? {})}`
      : !runtimeHelpOk
        ? `runtime bin/octopus usage check failed (status=${runtimeHelp?.status ?? "err"}): ${runtimeHelp?.error ? runtimeHelp.error.message : lastLines(runtimeHelp ?? {})}`
        : `launcher --help ok (0); runtime bin/octopus usage ok (${runtimeHelp.status})`;
    check("cli_help", launcherHelpOk && runtimeHelpOk, helpDetail);
  } else {
    check("cli_help", runtimeHelpOk, runtimeHelp?.error ? runtimeHelp.error.message : `status=${runtimeHelp?.status ?? "err"}: ${lastLines(runtimeHelp ?? {})}`);
  }
  if (fs.existsSync(path.join(home, "registry"))) {
    const doctor = spawn(bin, ["doctor"], { env: runtimeEnv(bundle, home, layout), encoding: "utf8", maxBuffer: 64 * 1024 * 1024 });
    check("doctor_home", doctor?.status === 0, doctor?.error ? doctor.error.message : lastLines(doctor ?? {}));
  }
  return { ok: checks.every((row) => row.ok), kind: layout.kind, bundle, build: info, checks };
}

async function verifyStaging(layout, home, {
  smoke = true,
  allowModifiedSource = false,
  smokePackages = [CHAT_CLIENT_PACKAGE, "world_provider_codex"],
  spawn = spawnSync,
  platform = "linux",
  serviceName = DEFAULT_SYSTEMD_SERVICE,
  authorityServiceName = DEFAULT_SYSTEMD_AUTHORITY_SERVICE,
  userService = false,
  systemctl = defaultSystemctl,
} = {}) {
  if (isCliLayout(layout)) return verifyCliStaging(layout, home, { spawn, platform, serviceName, authorityServiceName, userService, systemctl });
  const bundle = requireBundle(layout.staging, layout), checks = [];
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
    pollIntervalMs = DEFAULT_QUIET_POLL_MS, statusPollMs = 250, readTurns = readTurnsInFlight,
    serviceName = DEFAULT_SYSTEMD_SERVICE, authorityServiceName = DEFAULT_SYSTEMD_AUTHORITY_SERVICE,
    userService = false, requireAuthority = false, systemctl = defaultSystemctl } = options;
  const waitBudgetMs = nonNegativeMs(waitQuietMs, "waitQuietMs");
  const settleMs = nonNegativeMs(quietWindowMs, "quietWindowMs");
  const pollMs = positiveMs(pollIntervalMs, "pollIntervalMs");
  const statusPoll = positiveMs(statusPollMs, "statusPollMs");
  const before = await control(home, "status");
  if (before?.outcome !== "status") throw new Error(`no runtime answers for Home ${home} (${before?.error}); only a running runtime is restarted`);
  if (swap) requireBundle(layout.staging, layout);
  const systemd = isCliLayout(layout)
    ? inspectSystemdService(layout, home, { platform: options.platform ?? "linux", serviceName, authorityServiceName, userService, systemctl })
    : null;
  const refreshSystemd = () => (isCliLayout(layout)
    ? inspectSystemdService(layout, home, { platform: options.platform ?? "linux", serviceName, authorityServiceName, userService, systemctl })
    : null);
  const useSystemdRestart = Boolean(systemd?.loaded && systemd?.matchesTarget);
  const plan = [...(swap ? swapBundles(layout, { dryRun: true }).plan : []),
    useSystemdRestart ? `restart systemd unit ${serviceName}` : "restart through the control socket",
    "wait for a ready successor",
    rollback ? `on failure: restore ${layout.prev} and ${useSystemdRestart ? `restart systemd unit ${serviceName}` : "recover runtime"}` : "on failure: report only"];
  const refuse = (refused, extra = {}) => ({ ok: false, applied: false, refused, plan, ...extra });
  const usable = (status) => status?.outcome === "status" && status.phase === "ready" && status.stopping === null
    && sequence(status.started_at_unix_ms) && status.started_at_unix_ms > 0 && sequence(status.pid) && status.pid > 0
    && Array.isArray(status.interface_failures);
  if (!usable(before)) return refuse("runtime_not_ready");
  if (systemd?.loaded && !systemd.matchesTarget) {
    return refuse("systemd_service_mismatch", { systemd, error: systemd.reason });
  }
  if (isCliLayout(layout) && requireAuthority && systemd?.authorityService?.activeState !== "active") {
    return refuse("process_authority_inactive", {
      systemd,
      error: `${authorityServiceName} is not active (${systemd?.authorityService?.activeState ?? "unknown"})`,
    });
  }
  if (swap && isCliLayout(layout)) {
    const parity = checkCliLayoutParity(layout, systemd);
    if (!parity.ok) {
      return refuse("staging_layout_mismatch", { ...(systemd ? { systemd } : {}), error: parity.detail });
    }
  }
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
    const turns = await readTurns(layout.live, home, { sinceUnixMs: before.started_at_unix_ms, layout });
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
  if (dryRun) return { ok: true, applied: false, blocking, plan, runtime: before, ...(systemd ? { systemd } : {}), ...(waitSummary ?? {}) };
  if (swap) swapBundles(layout);
  const healthy = (status) => status?.outcome === "status" && status.phase === "ready" && status.stopping === null
    && Array.isArray(status.interface_failures) && newInterfaceFailures(before, status).length === 0;
  if (useSystemdRestart) {
    const restartRes = restartSystemdService(serviceName, { userService, systemctl });
    if (!restartRes.ok) {
      let rolledBack = false;
      if (swap && rollback && fs.existsSync(layout.prev)) {
        rollbackBundles(layout);
        rolledBack = true;
      }
      const afterFailedRestart = await control(home, "status", 2000);
      const stillHealthy = healthy(afterFailedRestart);
      const afterSystemd = refreshSystemd() ?? systemd;
      return {
        ok: false,
        applied: true,
        swapped: swap,
        rolled_back: rolledBack,
        recovered: stillHealthy,
        permission_denied: restartRes.permissionDenied === true,
        error: restartRes.error,
        systemd: afterSystemd,
        runtime: afterFailedRestart,
        ...(waitSummary ?? {}),
      };
    }
  } else {
    await control(home, "restart");
  }
  let lastUnitCheckAt = 0;
  const unitCheckIntervalMs = Math.min(1000, Math.max(statusPoll, 25));
  const crashAbortCheck = useSystemdRestart ? () => {
    const now = Date.now();
    if (lastUnitCheckAt !== 0 && now - lastUnitCheckAt < unitCheckIntervalMs) return null;
    lastUnitCheckAt = now;
    const probed = refreshSystemd();
    const reason = isSystemdUnitFailedOrCrashLooping(probed);
    return reason ? { reason, systemd: probed } : null;
  } : null;
  const first = await waitForStatus(
    home,
    (status) => healthy(status) && status.started_at_unix_ms !== before.started_at_unix_ms,
    timeoutMs,
    statusPoll,
    { abortCheck: crashAbortCheck },
  );
  if (first.ok) {
    const afterSystemd = refreshSystemd() ?? systemd;
    return { ok: true, applied: true, swapped: swap, previous: before, runtime: first.status, revision: buildInfo(layout.live, layout)?.revision ?? null, ...(afterSystemd ? { systemd: afterSystemd } : {}), ...(waitSummary ?? {}) };
  }
  const regressed = newInterfaceFailures(before, first.status);
  const crashMeta = first.aborted ? { crash_detected: true, abort_reason: first.abort.reason } : {};
  if (!rollback || !fs.existsSync(layout.prev)) {
    const afterSystemd = first.abort?.systemd ?? refreshSystemd() ?? systemd;
    return { ok: false, applied: true, swapped: swap, rolled_back: false, ...crashMeta, new_interface_failures: regressed, runtime: first.status, ...(afterSystemd ? { systemd: afterSystemd } : {}), ...(waitSummary ?? {}) };
  }
  rollbackBundles(layout);
  const current = await control(home, "status");
  const staleStartedAt = new Set([
    before.started_at_unix_ms,
    first.status?.outcome === "status" ? first.status.started_at_unix_ms : null,
    current?.outcome === "status" ? current.started_at_unix_ms : null,
  ].filter(sequence));
  if (useSystemdRestart) {
    const rbRestart = restartSystemdService(serviceName, { userService, systemctl });
    if (!rbRestart.ok) {
      const afterSystemd = refreshSystemd() ?? systemd;
      return {
        ok: false,
        applied: true,
        swapped: swap,
        rolled_back: true,
        recovered: false,
        permission_denied: rbRestart.permissionDenied === true,
        ...crashMeta,
        new_interface_failures: regressed,
        failed: first.status,
        runtime: current,
        systemd: afterSystemd,
        error: rbRestart.error,
        recovery: { mechanism: "systemd", service: serviceName, user_service: userService, restarted: false, error: rbRestart.error },
        ...(waitSummary ?? {}),
      };
    }
    const waited = await waitForStatus(home, (status) => healthy(status) && sequence(status.started_at_unix_ms)
      && !staleStartedAt.has(status.started_at_unix_ms), options.recoveryTimeoutMs ?? timeoutMs, statusPoll);
    const afterSystemd = refreshSystemd() ?? systemd;
    return {
      ok: false,
      applied: true,
      swapped: swap,
      rolled_back: true,
      recovered: waited.ok,
      ...crashMeta,
      new_interface_failures: regressed,
      failed: first.status,
      runtime: waited.status,
      systemd: afterSystemd,
      ...(waited.ok ? {} : { error: `runtime did not become ready for Home ${home} after rollback and systemctl restart ${serviceName}` }),
      recovery: { mechanism: "systemd", service: serviceName, user_service: userService, restarted: true },
      ...(waitSummary ?? {}),
    };
  }
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

async function status(layout, home, options = {}) {
  const describe = (bundle) => (fs.existsSync(bundle) ? buildInfo(bundle, layout) : null);
  const runtime = await control(home, "status");
  let turns = null;
  try {
    turns = (await readTurnsInFlight(layout.live, home, { sinceUnixMs: runtime.outcome === "status" ? runtime.started_at_unix_ms : 0, layout }))
      .filter((turn) => turn.turn_open);
  } catch (error) { turns = { error: error.message }; }
  const systemd = isCliLayout(layout)
    ? inspectSystemdService(layout, home, {
      platform: options.platform ?? "linux",
      serviceName: options.serviceName ?? DEFAULT_SYSTEMD_SERVICE,
      authorityServiceName: options.authorityServiceName ?? DEFAULT_SYSTEMD_AUTHORITY_SERVICE,
      userService: options.userService === true,
      systemctl: options.systemctl ?? defaultSystemctl,
    })
    : null;
  return { home, kind: layout.kind ?? "macos-app", install_dir: layout.live,
    head: spawnSync("git", ["rev-parse", "HEAD"], { cwd: repoRoot, encoding: "utf8" }).stdout.trim(), runtime,
    ...(systemd ? { systemd } : {}),
    bundles: { live: describe(layout.live), staging: describe(layout.staging), prev: describe(layout.prev), failed: describe(layout.failed) },
    turns_in_flight: turns };
}

const USAGE = `Usage: node app/scripts/safe-self-update.cjs <command> [options]
  status                          live runtime, bundle revisions, turns in flight (every run)
  build [--dry-run] [--min-free-gb=<GiB>] [--cli-target=<triple>]
                                  check free disk space (default ${DEFAULT_MIN_FREE_GB} GiB / OCTOPUS_SELF_UPDATE_MIN_FREE_GB) and stage into Octopus.staging.app or cli.staging
  prune [--dry-run] [--include-prev] [--include-cli] [--target-dir=<dir>]
                                  remove rebuildable staging/failed/Rust target artifacts without touching live install or ~/.octopus
  verify [--no-smoke] [--allow-modified-source]
  packages check [--json]          read-only drift and transfer preview
  packages available [--json]
  packages install <package-id>... [--dry-run] [--json]
  packages status | packages sync [--dry-run] [--with-chat-client] [--only=a,b]
  swap [--dry-run]                live -> prev, staging -> live
  restart [--swap] [--timeout=<seconds>] [--caller-run=<run id>] [--allow-in-flight-run <run id>]... [--wait-quiet[=<seconds>]] [--quiet-for=<seconds>] [--dry-run] [--force] [--no-rollback] [--detach --log=<file>]
                                  refuses on untrusted turn evidence, a changing frontier, or a blocking turn
  rollback [--dry-run]            prev -> live, live -> failed
Common: --home=<Octopus Home, default $OCTOPUS_HOME or ~/.octopus> [--state-dir=<dir>] --build-root=<dir>
Linux CLI / systemd: --platform=linux|darwin --target=cli|app --layout=cli|app --install-dir=<dir>
  --service=<unit, default ${DEFAULT_SYSTEMD_SERVICE}> --authority-service=<unit, default ${DEFAULT_SYSTEMD_AUTHORITY_SERVICE}> [--user-service] [--require-authority]
Note: value-taking options accept both --flag=value and --flag value.`;

const REPEATABLE_VALUE_OPTIONS = new Set(["allow-in-flight-run"]);
const VALUE_OPTIONS = new Set([
  "home",
  "state-dir",
  "build-root",
  "platform",
  "target",
  "layout",
  "install-dir",
  "service",
  "authority-service",
  "min-free-gb",
  "cli-target",
  "target-dir",
  "caller-run",
  "quiet-for",
  "only",
  "log",
  "timeout",
  "timeout-seconds",
]);
const OPTIONAL_NUMERIC_OPTIONS = new Set(["wait-quiet"]);
const DEFAULT_RESTART_TIMEOUT_MS = 300_000;

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

function resolveRestartTimeoutMs(options) {
  if (options.timeout !== undefined && options.timeoutSeconds !== undefined && options.timeout !== options.timeoutSeconds) {
    throw Object.assign(new Error("--timeout and --timeout-seconds conflict"), { usage: true });
  }
  if (options.timeout !== undefined) {
    return parseSecondsOption(options.timeout, "timeout", { min: 1, max: 3600 });
  }
  if (options.timeoutSeconds !== undefined) {
    return parseSecondsOption(options.timeoutSeconds, "timeout-seconds", { min: 1, max: 3600 });
  }
  return DEFAULT_RESTART_TIMEOUT_MS;
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
    if (VALUE_OPTIONS.has(name)) {
      const value = rest.length ? rest.join("=") : argv[++index];
      if (typeof value !== "string" || !value || (!rest.length && value.startsWith("--"))) {
        throw Object.assign(new Error(`--${name} requires a value`), { usage: true });
      }
      options[key] = value;
      continue;
    }
    if (OPTIONAL_NUMERIC_OPTIONS.has(name)) {
      if (rest.length) {
        options[key] = rest.join("=");
      } else if (index + 1 < argv.length && /^\d+(?:\.\d+)?$/.test(argv[index + 1])) {
        options[key] = argv[++index];
      } else {
        options[key] = true;
      }
      continue;
    }
    options[key] = rest.length ? rest.join("=") : true;
  }
  return { positional, options };
}

async function main(argv) {
  const { positional, options } = parseArgs(argv);
  if (positional[0] === "packages" && ["available", "install", "check"].includes(positional[1])) {
    const allowed = new Set(["home", "stateDir", "buildRoot", "platform", "target", "layout", "installDir", "json", "dryRun"]);
    for (const [key, value] of Object.entries(options)) {
      if (!allowed.has(key)) throw new Error(`Unknown packages option '${key}'`);
      if (["json", "dryRun"].includes(key) && value !== true) throw new Error(`--${key === "dryRun" ? "dry-run" : key} is a boolean flag, not a value`);
    }
    if (positional[1] === "check" && (positional.length !== 2 || options.dryRun)) throw new Error("packages check takes no package ids or --dry-run");
    if (positional[1] === "available" && positional.length !== 2) throw new Error("packages available takes no package ids");
  }
  const layout = bundles(
    options.buildRoot ? path.resolve(options.buildRoot) : undefined,
    {
      platform: options.platform !== undefined ? options.platform : process.platform,
      target: options.target,
      layout: options.layout,
      installDir: options.installDir,
    },
  );
  const home = path.resolve(options.home || options.stateDir || process.env.OCTOPUS_HOME || path.join(os.homedir(), ".octopus"));
  const serviceName = typeof options.service === "string" && options.service ? options.service : DEFAULT_SYSTEMD_SERVICE;
  const authorityServiceName = typeof options.authorityService === "string" && options.authorityService ? options.authorityService : DEFAULT_SYSTEMD_AUTHORITY_SERVICE;
  const userService = options.userService === true;
  const dryRun = options.dryRun === true, [command, sub] = positional;
  if (command === "status") return status(layout, home, { platform: options.platform ?? (isCliLayout(layout) ? "linux" : process.platform), serviceName, authorityServiceName, userService });
  if (command === "build") return buildStaging(layout, { dryRun, minFreeGb: options.minFreeGb, cliTarget: typeof options.cliTarget === "string" ? options.cliTarget : null });
  if (command === "prune") {
    if (options.targetDir === true || options.targetDir === "") {
      throw Object.assign(new Error("--target-dir requires a directory path"), { usage: true });
    }
    return pruneBuildArtifacts(layout, home, {
      dryRun,
      includePrev: options.includePrev === true,
      includeCli: options.includeCli === true,
      ...(typeof options.targetDir === "string" ? { targetDir: options.targetDir } : {}),
    });
  }
  if (command === "verify") {
    return verifyStaging(layout, home, {
      smoke: options.noSmoke !== true,
      allowModifiedSource: options.allowModifiedSource === true,
      platform: options.platform ?? (isCliLayout(layout) ? "linux" : process.platform),
      serviceName,
      authorityServiceName,
      userService,
    });
  }
  if (command === "packages" && sub === "check") return localPackages(layout.live).inspectLocalPackageDrift(packageInspectionRoots(layout.live, home));
  if (command === "packages" && sub === "available") return availablePackages(layout.live, home);
  if (command === "packages" && sub === "install") return installPackages(layout.live, home, positional.slice(2), { dryRun });
  if (command === "packages" && sub === "status") return { packages: await packageStatus(layout.live, home), baseline: baselineDrift(layout.live, home) };
  if (command === "packages" && sub === "sync") {
    return syncPackages(layout.live, home, { dryRun, withChatClient: options.withChatClient === true, only: typeof options.only === "string" ? options.only.split(",") : null });
  }
  if (command === "swap") return swapBundles(layout, { dryRun });
  if (command === "rollback") return rollbackBundles(layout, { dryRun });
  if (command === "restart") {
    const { waitQuietMs, quietWindowMs } = resolveQuietWaitOptions(options);
    const timeoutMs = resolveRestartTimeoutMs(options);
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
      timeoutMs, waitQuietMs, quietWindowMs, serviceName, authorityServiceName, userService,
      requireAuthority: options.requireAuthority === true,
      platform: options.platform ?? (isCliLayout(layout) ? "linux" : process.platform) });
  }
  throw Object.assign(new Error(USAGE), { usage: true });
}

module.exports = { bundles, resolveLayoutOptions, isCliLayout, isStandaloneCliTarget, resolveCliBundlePaths, cliReceipt, electronRoot, runtimeBinary,
  buildInfo, buildStaging, verifyStaging, isValidRuntimeUsageOutput, checkCliLayoutParity, status, checkBuildDiskSpace, resolveMinFreeGb, pruneBuildArtifacts,
  swapBundles, rollbackBundles, control, isReady, readChatTurns, readLedgerTurns, readTurnsInFlight, blockingTurns,
  newInterfaceFailures, packageAction, availablePackages, installPackages, restartRuntime, parseArgs, resolveQuietWaitOptions, resolveRestartTimeoutMs,
  readRuntimeInstanceState, parseLaunchdService, inspectLaunchdService,
  parseSystemctlShow, parseSystemctlExecStart, parseSystemctlEnvironment, inspectSystemdService, isSystemdUnitFailedOrCrashLooping, restartSystemdService,
  terminateProcess, runtimeEnv, main };

if (require.main === module) {
  main(process.argv.slice(2)).then((result) => {
    console.log(JSON.stringify(result, null, 2));
    process.exitCode = result?.ok === false ? 1 : 0;
  }, (error) => {
    if (process.argv.includes("--json")) { console.log(JSON.stringify({ ok: false, error: error.message }, null, 2)); process.exitCode = 2; return; }
    console.error(error.usage ? error.message : `safe-self-update: ${error.message}`); process.exitCode = 2; });
}
