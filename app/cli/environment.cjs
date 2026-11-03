const fs = require("node:fs");
const fsp = require("node:fs/promises");
const net = require("node:net");
const os = require("node:os");
const path = require("node:path");
const { spawn, execFile } = require("node:child_process");
const { parseEnv, promisify } = require("node:util");
const { setTimeout: delay } = require("node:timers/promises");
const { createRuntimeHost } = require("../electron/runtimeHost.cjs");
const { normalizedEndpoint } = require("../electron/interfaceTransport.cjs");
const { createChatgptSignIn } = require("../electron/chatgptSignIn.cjs");
const { createProjectionStore } = require("../electron/projection.cjs");
const { projectionReadRequest } = require("../electron/projectionReadContract.cjs");
const { recoverLocalPackageTransactions, registerLocalPackageMutationGuard } = require("../electron/localPackages.cjs");
const { recoverWorkspaceTransactions } = require("../electron/workspaceSetup.cjs");
const { commitProjectionClientAuthoring } = require("../electron/clientWorkspaceAuthoring.cjs");
const { createOperatorChatClientLoader } = require("../electron/operatorChatClientContract.cjs");

const privateEnvironments = new WeakMap();
const starts = new Map();
const PACKAGE_COMMAND = "npm --prefix app run build:cli (or node app/scripts/package-cli.cjs to package existing release binaries)";
const execute = promisify(execFile);

function taskRetirementBarrier(environment) {
  const load = async () => {
    const registryRoot = environment.packageRoots.activeRegistryRoot;
    const loader = createOperatorChatClientLoader({ packageRoots: () => environment.packageRoots,
      clientPath: () => path.join(registryRoot, "interface", "world_operator_chat", "client.cjs") });
    const client = await loader.load();
    return (await client.loadOperatorTaskLifecycle({ registryRoot: () => registryRoot,
      installationRoot: () => environment.installationRoot })).retirementBarrier;
  };
  return {
    verifyBefore: (context) => load().then((barrier) => barrier.verifyBefore(context)),
    withHeldProof: (context, operation) => load().then((barrier) => barrier.withHeldProof(context, operation)),
  };
}

function resolveEnvironment(options = {}) {
  const ambient = { ...(options.env ?? process.env) };
  const userRoot = absolute(options.userHome ?? os.homedir(), "user home");
  const requestedHome = options.home === undefined ? null : argumentPath(options.home, options.cwd, userRoot, "--home");
  const initialHome = requestedHome ?? absolute(ambient.OCTOPUS_HOME || path.join(userRoot, ".octopus"), "OCTOPUS_HOME");
  const fileEnvironment = readEnvironmentFile(path.join(initialHome, ".env"));
  const merged = { ...fileEnvironment, ...ambient };
  const home = requestedHome ?? absolute(merged.OCTOPUS_HOME || initialHome, "OCTOPUS_HOME");
  const repoRoot = absolute(options.repoRoot ?? merged.OCTOPUS_REPO_ROOT ?? path.resolve(__dirname, "../.."), "repository root");
  if (options.bundle && options.installationRoot) throw new Error("Choose either --bundle or installationRoot.");
  const installationRoot = options.bundle
    ? bundleRoot(argumentPath(options.bundle, options.cwd, userRoot, "--bundle"))
    : absolute(options.installationRoot ?? merged.OCTOPUS_INSTALLATION_ROOT ?? path.join(repoRoot, "app", "build", "cli", "runtime"), "installation root");
  const sourceRegistry = absolute(options.sourceRegistry ?? merged.OCTOPUS_SOURCE_REGISTRY ?? path.join(installationRoot, "registry"), "source registry");
  const repoInstallation = installationRoot === repoRoot;
  const runtimeBinary = absolute(options.runtimeBinary ?? merged.OCTOPUS_RUNTIME_BIN
    ?? path.join(installationRoot, ...(repoInstallation ? ["target", "release"] : ["bin"]), "octopus"), "runtime binary");
  if (runtimeBinary.split(path.sep).some((part, index, parts) => part === "debug" && parts[index - 1] === "target")) {
    throw new Error("The terminal client requires a release runtime; target/debug is not supported.");
  }
  const codexHome = absolute(options.codexHome ?? merged.CODEX_HOME ?? path.join(userRoot, ".codex"), "CODEX_HOME");
  if (options.codexBinary && options.codexBundle) throw new Error("Choose either codexBinary or codexBundle.");
  const codexBinary = absolute(options.codexBinary ?? merged.OCTOPUS_CODEX_BIN
    ?? (options.codexBundle ? path.join(bundleRoot(argumentPath(options.codexBundle, options.cwd, userRoot, "Codex bundle")), "bin", "codex")
      : repoInstallation ? path.join(sourceRegistry, "interface", "world_provider_codex", "packages", "bin", "codex")
        : path.join(installationRoot, "bin", "codex")), "Codex binary");
  const packageRoots = {
    sourceRegistryRoot: absolute(options.localPackageRegistry ?? merged.OCTOPUS_LOCAL_PACKAGE_REGISTRY
      ?? (repoInstallation ? sourceRegistry : path.join(installationRoot, "local-packages", "registry")), "local package registry"),
    activeRegistryRoot: path.join(home, "registry"),
    libraryRoot: absolute(options.localPackageLibrary ?? merged.OCTOPUS_LOCAL_PACKAGE_LIBRARY
      ?? (repoInstallation ? path.join(repoRoot, "app", "distribution", "local-packages") : path.join(installationRoot, "local-packages")), "local package library"),
    stateRoot: path.join(home, "client", "package-state"),
  };
  packageRoots.publication = require("../electron/packageStorage.cjs").createPackageStoragePublisher(packageRoots.sourceRegistryRoot, installationRoot);
  const launchEnv = {
    OCTOPUS_HOME: home,
    OCTOPUS_SOURCE_REGISTRY: sourceRegistry,
    OCTOPUS_INSTALLATION_ROOT: installationRoot,
    OCTOPUS_RUNTIME_BIN: runtimeBinary,
    OCTOPUS_MACHINE_CODEX_AUTH_FILE: path.join(codexHome, "auth.json"),
    OCTOPUS_MACHINE_CODEX_AUTH_LOCK_FILE: path.join(codexHome, "auth.lock"),
    PATH: [path.dirname(runtimeBinary), path.join(installationRoot, "bin"), merged.PATH ?? ""].join(path.delimiter),
  };
  const endpoint = normalizedEndpoint(options.endpoint ?? merged.OCTOPUS_APP_LISTEN ?? "", home, "world_operator_chat");
  if (!endpoint.startsWith("unix://")) throw new Error("The local macOS terminal client requires a Unix runtime endpoint.");
  requireSocketLength(endpoint.slice(7));
  const result = {
    home, repoRoot, installationRoot, sourceRegistry, runtimeBinary, launchEnv, endpoint, packageRoots,
    codexHome, codexBinary, logPath: path.join(home, "client", "logs", "runtime-terminal.log"),
    launchCwd: repoInstallation ? repoRoot : userRoot,
  };
  const childEnv = { ...merged, ...launchEnv, CODEX_HOME: codexHome };
  delete childEnv.OCTOPUS_CLIENT_ADMIN_SECRET;
  delete childEnv.OCTOPUS_CLIENT_LAB_SECRET;
  delete childEnv.OCTOPUS_APP_LISTEN;
  if (options.endpoint || merged.OCTOPUS_APP_LISTEN) childEnv.OCTOPUS_APP_LISTEN = endpoint;
  privateEnvironments.set(result, { childEnv, configuredEndpoint: Boolean(options.endpoint || merged.OCTOPUS_APP_LISTEN) });
  return result;
}

function createEnvironment(options = {}) {
  const resolved = resolveEnvironment(options);
  let account = null;
  let statusProjection = null;
  let signInHandler = options.openExternal ?? options.ui?.openExternal;
  const environment = {
    ...resolved,
    async status(request = {}) { return status(environment, request, () => statusProjection ??= projectionStore(environment)); },
    async start(request = {}) { return start(environment, request); },
    async setup(request = {}) { return require("./setup.cjs").setupEnvironment(environment, request); },
    accountClient(request = {}) {
      if (request.openExternal !== undefined) {
        if (typeof request.openExternal !== "function") throw new Error("Sign-in address handler must be a function.");
        signInHandler = request.openExternal;
      }
      if (!account) account = createChatgptSignIn({
        codexBinary: () => { requireExecutable(environment.codexBinary, "managed Codex"); return environment.codexBinary; },
        codexHome: () => environment.codexHome,
        version: () => require("../package.json").version,
        openExternal: async (url) => {
          if (typeof signInHandler !== "function") throw new Error("ChatGPT sign-in requires an interactive address handler. Run setup in a terminal.");
          return signInHandler(url);
        },
      });
      return account;
    },
    close() {
      if (account) account.dispose();
      account = null;
      statusProjection?.close();
      statusProjection = null;
    },
  };
  privateEnvironments.set(environment, privateEnvironments.get(resolved));
  return environment;
}

async function status(environment, options = {}, currentProjection) {
  const host = runtimeHost(environment);
  const observed = await host.probe();
  const result = {
    phase: observed.phase, runtimeReady: observed.ready, clientReady: false, ready: false,
    pid: observed.pid, ownership: observed.ownership, home: environment.home,
    endpoint: environment.endpoint, logPath: environment.logPath,
    storageHealth: observed.storageHealth,
    interfaceFailures: observed.interfaceFailures,
    reason: observed.error ?? (observed.ready ? "Home setup has not been verified." : "Octopus runtime is not ready."),
  };
  if (!observed.ready) return result;
  let interfaceId = options.interfaceId;
  if (interfaceId === undefined) {
    const store = currentProjection ? currentProjection() : projectionStore(environment);
    try {
      const scope = await store.read(projectionReadRequest("projection.operator.os.scope"));
      if (scope.status === "invalid") {
        result.reason = `Home scope is invalid: ${(scope.errors ?? []).join("; ")}`;
        return result;
      }
      if (scope.status !== "ready" || !scope.home_interface_id) {
        result.reason = "Home is not configured. Run setup.";
        return result;
      }
      interfaceId = scope.home_interface_id;
    } catch (error) {
      result.reason = `Home scope cannot be read: ${error.message}`;
      return result;
    } finally { if (!currentProjection) store.close(); }
  }
  if (typeof interfaceId !== "string" || !/^[A-Za-z0-9_-]{1,96}$/.test(interfaceId)) throw new Error("Home endpoint requires an exact interface id.");
  result.endpoint = privateEnvironments.get(environment)?.configuredEndpoint
    ? environment.endpoint : normalizedEndpoint("", environment.home, interfaceId);
  requireSocketLength(result.endpoint.slice(7));
  result.clientReady = await socketAccepts(result.endpoint.slice(7), options.probeTimeoutMs ?? 250);
  result.ready = result.runtimeReady && result.clientReady;
  result.reason = result.clientReady ? null : `Home interface '${interfaceId}' is not accepting connections yet.`;
  if (result.clientReady) {
    const confirmed = await host.probe();
    if (!confirmed.ready || confirmed.pid !== result.pid) {
      result.runtimeReady = false;
      result.ready = false;
      result.reason = "Runtime changed while the Home endpoint was being checked. Retry status.";
    }
  }
  return result;
}

async function start(environment, options = {}) {
  const timeoutMs = positiveTimeout(options.timeoutMs ?? 120_000);
  const onProgress = typeof options.onProgress === "function" ? options.onProgress : () => {};
  assertNotAborted(options.signal);
  const host = runtimeHost(environment);
  const current = await host.probe();
  if (current.error) throw runtimeError(current.error, environment);
  if (current.ready) {
    onProgress({ phase: "reusing_runtime", pid: current.pid, home: environment.home, logPath: environment.logPath });
    return options.requireEndpoint === true ? waitForClient(environment, options) : status(environment, options);
  }
  const pending = starts.get(environment.home);
  if (pending) {
    if (pending.binary !== environment.runtimeBinary || pending.source !== environment.sourceRegistry) throw runtimeError("Another terminal startup for this Home has different installation paths.", environment);
    onProgress({ phase: "joining_runtime", home: environment.home, logPath: environment.logPath });
    await pending.promise;
    assertNotAborted(options.signal);
    return options.requireEndpoint === true ? waitForClient(environment, options) : status(environment, options);
  }
  const promise = launchRuntime(environment, { ...options, timeoutMs, onProgress });
  starts.set(environment.home, { promise, binary: environment.runtimeBinary, source: environment.sourceRegistry });
  try { await promise; }
  finally { if (starts.get(environment.home)?.promise === promise) starts.delete(environment.home); }
  return options.requireEndpoint === true ? waitForClient(environment, options) : status(environment, options);
}

async function launchRuntime(environment, options) {
  const host = runtimeHost(environment);
  let observed = await host.probe();
  if (observed.error) throw runtimeError(observed.error, environment);
  let child = null;
  let exit = null;
  if (observed.pid == null) {
    requireExecutable(environment.runtimeBinary, "release runtime");
    requireDistribution(environment);
    const assertStopped = async () => {
        assertNotAborted(options.signal);
        const current = await host.probe();
        if (current.phase !== "unmanaged" || current.ready || current.ownership !== "none" || current.pid !== null || current.error) {
          throw runtimeError("Close the running Octopus runtime before recovering its interrupted workspace transactions.", environment);
        }
      };
    const releaseGuard = registerLocalPackageMutationGuard({ stateRoot: environment.packageRoots.stateRoot, guard: assertStopped });
    try {
      await recoverLocalPackageTransactions(environment.packageRoots);
      await assertStopped();
      await recoverWorkspaceTransactions({ octopusHomeRoot: environment.home, registryRoot: environment.packageRoots.activeRegistryRoot,
        publication: environment.packageRoots.publication, retirementBarrier: taskRetirementBarrier(environment) });
    } finally { releaseGuard(); }
    assertNotAborted(options.signal);
    await fsp.mkdir(path.dirname(environment.logPath), { recursive: true, mode: 0o700 });
    const log = await fsp.open(environment.logPath, fs.constants.O_WRONLY | fs.constants.O_CREAT | fs.constants.O_APPEND | fs.constants.O_NOFOLLOW, 0o600);
    try {
      const stat = await log.stat();
      if (!stat.isFile() || stat.nlink !== 1) throw new Error("Runtime log must be a regular private file.");
      await log.chmod(0o600);
      await log.write(`\n${new Date().toISOString()} Starting ${environment.runtimeBinary} up\n`);
      options.onProgress({ phase: "starting_runtime", command: `${environment.runtimeBinary} up`, home: environment.home, logPath: environment.logPath });
      child = spawn(environment.runtimeBinary, ["up"], {
        cwd: environment.launchCwd,
        env: privateEnvironments.get(environment)?.childEnv ?? { ...process.env, ...environment.launchEnv },
        detached: true, stdio: ["ignore", log.fd, log.fd],
      });
      child.once("error", (error) => { exit = { error }; });
      child.once("exit", (code, signal) => { exit = { code, signal }; });
      child.unref();
    } finally { await log.close(); }
  } else options.onProgress({ phase: "joining_runtime", pid: observed.pid, home: environment.home, logPath: environment.logPath });
  const deadline = Date.now() + options.timeoutMs;
  let phase = null;
  let exitedAt = null;
  let sawPid = observed.pid != null;
  while (Date.now() < deadline) {
    assertNotAborted(options.signal);
    observed = await host.probe();
    if (observed.error) throw runtimeError(observed.error, environment);
    if (observed.phase !== phase && observed.pid != null) {
      phase = observed.phase;
      options.onProgress({ phase, pid: observed.pid, home: environment.home, logPath: environment.logPath });
    }
    if (observed.ready) return observed;
    if (observed.pid != null) sawPid = true;
    if (exit && exitedAt == null) exitedAt = Date.now();
    if (exit && observed.pid == null && Date.now() - exitedAt >= 500) {
      const detail = exit.error?.message ?? `exit ${exit.code ?? exit.signal ?? "unknown"}`;
      throw runtimeError(`Octopus runtime stopped before readiness (${detail}).`, environment);
    }
    if (sawPid && observed.pid == null && !child) throw runtimeError("The runtime being joined exited before readiness.", environment);
    await delay(100, undefined, { signal: options.signal });
  }
  throw runtimeError(`Octopus runtime did not become ready in ${Math.round(options.timeoutMs / 1000)} seconds. Last phase: ${observed.phase}.`, environment);
}

async function waitForClient(environment, options) {
  const deadline = Date.now() + positiveTimeout(options.timeoutMs ?? 120_000);
  let observed;
  do {
    assertNotAborted(options.signal);
    observed = await status(environment, options);
    if (observed.ready) return observed;
    if (!observed.runtimeReady || observed.reason === "Home is not configured. Run setup." || observed.reason.startsWith("Home scope")) throw runtimeError(observed.reason, environment);
    await delay(150, undefined, { signal: options.signal });
  } while (Date.now() < deadline);
  throw runtimeError(observed.reason, environment);
}

function runtimeHost(environment) {
  return createRuntimeHost({
    home: () => environment.home, sourceRegistry: () => environment.sourceRegistry,
    runtimeBinary: () => environment.runtimeBinary, repoRoot: () => environment.repoRoot,
    launchCwd: () => environment.launchCwd, launchEnv: () => environment.launchEnv,
    launchCommand: () => `${environment.runtimeBinary} up`, endpoint: () => environment.endpoint.slice(7),
  });
}

function projectionStore(environment) {
  return createProjectionStore({
    resolveRepoRoot: () => environment.repoRoot, sourceRegistry: () => environment.sourceRegistry,
    localPackageRegistry: () => environment.packageRoots.sourceRegistryRoot,
    localPackageLibrary: () => environment.packageRoots.libraryRoot,
    octopusHome: () => environment.home, runtimeLaunchEnvironment: () => environment.launchEnv,
    projectEePolicies: async () => {
      const { stdout } = await execute(environment.runtimeBinary, ["project-ee-policies"], {
        cwd: environment.installationRoot,
        env: privateEnvironments.get(environment)?.childEnv ?? { ...process.env, ...environment.launchEnv },
        maxBuffer: 32 * 1024 * 1024, timeout: 120_000,
      });
      return JSON.parse(stdout);
    },
  });
}

function readEnvironmentFile(file) {
  let stat;
  try { stat = fs.lstatSync(file); }
  catch (error) { if (error.code === "ENOENT") return {}; throw new Error(`Client environment cannot be inspected: ${error.message}`); }
  if (!stat.isFile() || stat.isSymbolicLink() || stat.size <= 0 || stat.size > 64 * 1024) throw new Error("Client environment must be a bounded regular .env file.");
  if (process.platform !== "win32" && (stat.mode & 0o077)) throw new Error("Client environment file permissions are too broad; expected mode 0600.");
  return parseEnv(fs.readFileSync(file, "utf8"));
}

function requireDistribution(environment) {
  for (const root of [environment.sourceRegistry, environment.packageRoots.sourceRegistryRoot]) {
    if (!fs.existsSync(path.join(root, ".octopus-source.json")) || !fs.existsSync(path.join(root, "interface"))) throw new Error(`Terminal distribution is incomplete at '${root}'. Build it with: ${PACKAGE_COMMAND}`);
  }
  for (const name of ["packages", "packs"]) {
    if (!fs.existsSync(path.join(environment.packageRoots.libraryRoot, name))) throw new Error(`Terminal package library is incomplete. Build it with: ${PACKAGE_COMMAND}`);
  }
}

function requireExecutable(file, label) {
  try {
    const stat = fs.statSync(file);
    if (!stat.isFile()) throw new Error("not a file");
    fs.accessSync(file, fs.constants.X_OK);
  } catch { throw new Error(`The ${label} binary is unavailable at '${file}'. Build the terminal distribution with: ${PACKAGE_COMMAND}`); }
}

function bundleRoot(root) { return root.endsWith(".app") ? path.join(root, "Contents", "Resources", "app") : root; }
function absolute(value, label) {
  if (typeof value !== "string" || !value.trim() || value.includes("\0") || !path.isAbsolute(value)) throw new Error(`${label} must be an absolute path.`);
  return path.resolve(value);
}
function argumentPath(value, cwd = process.cwd(), userRoot = os.homedir(), label) {
  if (typeof value !== "string" || !value.trim() || value.includes("\0")) throw new Error(`${label} requires a path.`);
  return path.resolve(cwd, value === "~" ? userRoot : value.startsWith("~/") ? path.join(userRoot, value.slice(2)) : value);
}
function requireSocketLength(file) {
  const limit = process.platform === "darwin" ? 103 : 107;
  if (Buffer.byteLength(file) > limit) throw new Error(`Octopus Home is too long for a Unix socket (${limit} bytes maximum). Choose a shorter --home.`);
}
function socketAccepts(file, timeoutMs) {
  return new Promise((resolve) => {
    const socket = net.createConnection(file);
    const timer = setTimeout(() => finish(false), positiveTimeout(timeoutMs));
    let settled = false;
    function finish(value) { if (settled) return; settled = true; clearTimeout(timer); socket.destroy(); resolve(value); }
    socket.once("connect", () => finish(true));
    socket.once("error", () => finish(false));
  });
}
function positiveTimeout(value) {
  if (!Number.isSafeInteger(value) || value < 1) throw new Error("Timeout must be a positive integer in milliseconds.");
  return value;
}
function assertNotAborted(signal) { if (signal?.aborted) { const error = new Error("Terminal operation cancelled; any shared runtime already started remains running."); error.code = "ABORT_ERR"; throw error; } }
function runtimeError(message, environment) { const error = new Error(`${message} Runtime log: ${environment.logPath}`); error.logPath = environment.logPath; return error; }

module.exports = { taskRetirementBarrier, resolveEnvironment, createEnvironment, status, start, projectionStore, requireDistribution };
