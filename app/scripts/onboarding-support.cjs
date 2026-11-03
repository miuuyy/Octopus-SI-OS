const crypto = require("node:crypto");
const fs = require("node:fs");
const path = require("node:path");
const { pathToFileURL } = require("node:url");
const { spawn, spawnSync } = require("node:child_process");
const WebSocket = require("ws");
const { readGeneratedSourcePaths, isGeneratedSourcePath } = require("./generated-source-boundary.cjs");
const { verifyNodeHeaders } = require("../native/runtime.cjs");

const EXIT = Object.freeze({ passed: 0, arguments: 10, build: 11, package: 12, launch: 20, welcome: 21, signIn: 22, boot: 23, password: 24, setup: 25, home: 26, homeReply: 27, task: 28, interrupted: 130 });
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const sha = (bytes) => crypto.createHash("sha256").update(bytes).digest("hex");
const stopped = (child) => child.exitCode !== null || child.signalCode !== null;

function failure(message, exitCode) {
  return Object.assign(new Error(message), { safeMessage: message, exitCode });
}

function parseArguments(args) {
  const options = { mode: "unauthenticated", noBuild: false, timeoutMs: 600000 };
  for (let i = 0; i < args.length; i++) {
    const flag = args[i];
    if (flag === "--help") return { help: true };
    if (flag === "--no-build") { options.noBuild = true; continue; }
    if (!["--mode", "--auth-file", "--target", "--timeout-ms"].includes(flag) || !args[i + 1] || args[i + 1].startsWith("--")) throw failure(`Unknown or incomplete onboarding argument: ${flag}`, EXIT.arguments);
    const value = args[++i];
    if (flag === "--mode") options.mode = value;
    if (flag === "--auth-file") {
      if (!path.isAbsolute(value)) throw failure("--auth-file must name one absolute credential file.", EXIT.arguments);
      options.authFile = value;
    }
    if (flag === "--target") options.target = value;
    if (flag === "--timeout-ms") options.timeoutMs = Number(value);
  }
  if (!["unauthenticated", "reused-auth"].includes(options.mode)) throw failure("Mode must be unauthenticated or reused-auth.", EXIT.arguments);
  if ((options.mode === "reused-auth") !== Boolean(options.authFile)) throw failure("Only reused-auth mode accepts and requires an explicit --auth-file.", EXIT.arguments);
  options.target ||= options.mode === "unauthenticated" ? "sign-in" : "task";
  if (!["sign-in", "home-reply", "task"].includes(options.target) || (options.target === "sign-in") !== (options.mode === "unauthenticated")) throw failure("Unauthenticated mode verifies sign-in; reused-auth mode verifies home-reply or task.", EXIT.arguments);
  if (!Number.isSafeInteger(options.timeoutMs) || options.timeoutMs < 30000 || options.timeoutMs > 1800000) throw failure("Timeout must be 30000 through 1800000 milliseconds.", EXIT.arguments);
  return options;
}

function createRedactor(secrets = []) {
  const values = [...new Set(secrets.filter((value) => typeof value === "string" && value.length >= 8))].sort((a, b) => b.length - a.length);
  return (value) => {
    let text = String(value);
    for (const secret of values) text = text.split(secret).join("[REDACTED]");
    return text
      .replace(/Bearer\s+[^\s"<>]+/gi, "Bearer [REDACTED]")
      .replace(/((?:access_token|refresh_token|id_token|OPENAI_API_KEY|client_secret|password|admin_secret|lab_secret)["']?\s*[:=]\s*["']?)[^\s"',}]+/gi, "$1[REDACTED]")
      .replace(/https?:\/\/[^\s"<>]*(?:oauth|authorize|auth\/callback|[?&](?:code|state|token|code_challenge)=)[^\s"<>]*/gi, "[REDACTED SIGN-IN URL]");
  };
}

function collectStrings(value, result = []) {
  if (typeof value === "string") result.push(value);
  else if (Array.isArray(value)) value.forEach((entry) => collectStrings(entry, result));
  else if (value && typeof value === "object") Object.values(value).forEach((entry) => collectStrings(entry, result));
  return result;
}

function readSelectedAuth(file) {
  const fd = fs.openSync(file, fs.constants.O_RDONLY | fs.constants.O_NOFOLLOW);
  try {
    const stat = fs.fstatSync(fd);
    if (!stat.isFile() || stat.size > 1048576) throw failure("The selected credential must be a bounded regular auth.json file.", EXIT.arguments);
    const bytes = fs.readFileSync(fd);
    const auth = JSON.parse(bytes);
    if (auth?.auth_mode !== "chatgpt" || !auth.tokens || typeof auth.tokens.access_token !== "string" || !auth.tokens.access_token) throw failure("The selected credential does not contain a ChatGPT sign-in.", EXIT.arguments);
    return { bytes, redact: createRedactor(collectStrings(auth)) };
  } finally { fs.closeSync(fd); }
}

function writeJson(file, value, redact = createRedactor()) {
  const temporary = `${file}.tmp`;
  fs.writeFileSync(temporary, redact(JSON.stringify(value, null, 2)) + "\n", { mode: 0o600 });
  fs.renameSync(temporary, file);
}

function sourceFingerprint(repoRoot) {
  const generated = readGeneratedSourcePaths(repoRoot);
  const hash = crypto.createHash("sha256");
  const fileDigests = {};
  let files = 0;
  function visit(relative) {
    if (isGeneratedSourcePath(relative, generated)) return;
    const absolute = path.join(repoRoot, relative);
    const stat = fs.lstatSync(absolute);
    if (stat.isDirectory()) {
      for (const name of fs.readdirSync(absolute).sort()) {
        if ([".git", "target", "node_modules", ".build", ".swiftpm", ".DS_Store"].includes(name)) continue;
        visit(path.join(relative, name));
      }
    } else if (stat.isFile() || stat.isSymbolicLink()) {
      if (++files > 50000) throw failure("Build source inventory exceeds its bound.", EXIT.build);
      const name = relative.split(path.sep).join("/");
      const bytes = stat.isSymbolicLink() ? fs.readlinkSync(absolute) : fs.readFileSync(absolute);
      fileDigests[name] = sha(bytes);
      hash.update(name + "\0");
      hash.update(bytes);
      hash.update("\0");
    } else throw failure(`Unsupported build input: ${relative}`, EXIT.build);
  }
  for (const root of ["Cargo.toml", "Cargo.lock", ".cargo", "crates", "registry", "schemas", "app/index.html", "app/src", "app/electron", "app/scripts", "app/public", "app/schemas", "app/distribution", "app/publication", "app/licenses", "app/client-surfaces", "app/package.json", "app/package-lock.json", "app/vite.config.ts", "app/tsconfig.json"]) {
    if (fs.existsSync(path.join(repoRoot, root))) visit(root);
  }
  return { sha256: hash.digest("hex"), fileCount: files, fileDigests, declaredGeneratedOutputs: generated };
}

function changedInputs(before, after) {
  const oldFiles = before.fileDigests || {};
  const newFiles = after.fileDigests || {};
  return [...new Set([...Object.keys(oldFiles), ...Object.keys(newFiles)])].sort().flatMap((file) => oldFiles[file] === newFiles[file] ? [] : [{ path: file, before: oldFiles[file] || null, after: newFiles[file] || null }]);
}

function captureLog(stream, file, redact, onSafeLine) {
  let pending = "";
  let total = 0;
  fs.writeFileSync(file, "", { mode: 0o600 });
  const flush = (value) => {
    if (total >= 16777216) return;
    const safe = redact(value);
    total += Buffer.byteLength(safe);
    fs.appendFileSync(file, safe);
    onSafeLine?.(safe);
  };
  stream.setEncoding("utf8");
  stream.on("data", (chunk) => {
    pending += chunk;
    const newline = pending.lastIndexOf("\n");
    if (newline >= 0) { flush(pending.slice(0, newline + 1)); pending = pending.slice(newline + 1); }
    if (pending.length > 1048576) { flush("[oversized log line omitted]\n"); pending = ""; }
  });
  stream.on("end", () => { flush(pending); pending = ""; });
}

async function processExit(child, timeoutMs, exitCode = EXIT.launch) {
  if (stopped(child)) return { code: child.exitCode, signal: child.signalCode };
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(failure("Owned process exceeded the onboarding timeout.", exitCode)), timeoutMs);
    child.once("error", (error) => { clearTimeout(timer); reject(error); });
    child.once("exit", (code, signal) => { clearTimeout(timer); resolve({ code, signal }); });
  });
}

function verifyBundle(bundleRoot, reportRoot) {
  const executable = path.join(bundleRoot, "Contents/MacOS/Octopus");
  const appRoot = path.join(bundleRoot, "Contents/Resources/app");
  for (const file of [executable, path.join(appRoot, "bin/octopus"), path.join(appRoot, "bin/codex"), path.join(appRoot, "dist/index.html"), path.join(appRoot, "registry/.octopus-source.json"), path.join(appRoot, "local-packages/registry/.octopus-source.json")]) {
    if (!fs.existsSync(file) || !fs.statSync(file).isFile() || !fs.realpathSync(file).startsWith(fs.realpathSync(bundleRoot) + path.sep)) throw failure(`Packaged onboarding input is missing or escapes the bundle: ${file}`, EXIT.package);
  }
  const signing = spawnSync("codesign", ["--verify", "--deep", "--strict", bundleRoot], { encoding: "utf8" });
  fs.writeFileSync(path.join(reportRoot, "codesign.log"), String(signing.stdout || "") + String(signing.stderr || ""), { mode: 0o600 });
  if (signing.error || signing.status !== 0) throw failure("The new packaged app did not pass strict code signing verification.", EXIT.package);
  return { executable, appRoot, executableSha256: sha(fs.readFileSync(executable)), runtimeSha256: sha(fs.readFileSync(path.join(appRoot, "bin/octopus"))), resourcesSha256: sha(fs.readFileSync(path.join(bundleRoot, "Contents/_CodeSignature/CodeResources"))) };
}

async function runFreshOnboarding(args) {
  const options = parseArguments(args);
  if (options.help) {
    console.log("Fresh packaged onboarding (macOS):\n  npm run dev:onboarding -- --mode unauthenticated\n  npm run dev:onboarding -- --mode reused-auth --auth-file /absolute/path/auth.json\nOptions: --target home-reply|task, --timeout-ms 600000, --no-build\nDefault: build/sign this repository's app/build/Octopus.app, create an empty isolated Home, walk the UI.\n--no-build requires an unchanged source fingerprint and a previous successful harness build.\nExit codes: 0 requested boundary passed; 10 arguments; 11 build; 12 package; 20 launch; 21 welcome; 22 sign-in; 23 boot; 24 password; 25 setup; 26 Home; 27 Home reply; 28 task; 130 interrupted.\nReports: ignored .private/onboarding/<run>/report.json. Selected auth values are never logged; only auth.json is reused, and its test copy is removed after the run.");
    return;
  }
  if (process.platform !== "darwin") throw failure("The packaged macOS onboarding harness requires macOS.", EXIT.arguments);
  const repoRoot = path.resolve(__dirname, "../..");
  const appRoot = path.join(repoRoot, "app");
  const privateRoot = path.join(repoRoot, ".private/onboarding");
  const ignored = spawnSync("git", ["check-ignore", "--quiet", path.join(privateRoot, "report.json")], { cwd: repoRoot });
  if (ignored.status !== 0) throw failure(".private/onboarding must be git-ignored before the harness writes evidence.", EXIT.arguments);
  const credential = options.authFile ? readSelectedAuth(options.authFile) : null;
  const redact = credential?.redact || createRedactor();
  const runId = new Date().toISOString().replace(/[:.]/g, "-") + "-" + crypto.randomBytes(3).toString("hex");
  const reportRoot = path.join(privateRoot, runId);
  fs.mkdirSync(reportRoot, { recursive: true, mode: 0o700 });
  const reportFile = path.join(reportRoot, "report.json");
  let report = { schema_version: 1, runId, mode: options.mode, target: options.target, status: "running", boundary: "build", startedAt: new Date().toISOString(), checks: [], reportRoot };
  let child = null;
  let walker = null;
  let isolatedRoot = null;
  let interrupted = false;
  let browserUrl = "";
  const ownedGroups = new Set();
  const stop = () => { interrupted = true; walker?.kill("SIGTERM"); child?.kill("SIGTERM"); };
  process.once("SIGINT", stop);
  process.once("SIGTERM", stop);
  writeJson(reportFile, report, redact);
  console.log(`Fresh onboarding ${options.mode} → ${options.target}\nReport: ${reportFile}`);
  try {
    const fingerprint = sourceFingerprint(repoRoot);
    const bundleRoot = path.join(appRoot, "build/Octopus.app");
    const buildProofFile = path.join(privateRoot, "build.json");
    if (!options.noBuild) {
      if (Number(process.versions.node.split(".")[0]) < 22) throw failure("Use an installed Node 22 or newer runtime for the package build.", EXIT.build);
      try { verifyNodeHeaders(process.env.OCTOPUS_NATIVE_NODE_HEADERS); } catch (error) { throw failure(`The package builder requires explicit OCTOPUS_NATIVE_NODE_HEADERS: ${error.message}`, EXIT.build); }
      const npmCli = process.env.npm_execpath;
      report.buildAttempt = { source: fingerprint, command: ["npm", "run", "package:mac"], stdout: path.join(reportRoot, "build.stdout.log"), stderr: path.join(reportRoot, "build.stderr.log") };
      writeJson(reportFile, report, redact);
      const build = spawn(npmCli ? process.execPath : "npm", npmCli ? [npmCli, "run", "package:mac"] : ["run", "package:mac"], { cwd: appRoot, env: { ...process.env, PATH: [path.dirname(process.execPath), process.env.PATH || ""].join(path.delimiter) }, detached: true, stdio: ["ignore", "pipe", "pipe"] });
      captureLog(build.stdout, path.join(reportRoot, "build.stdout.log"), redact);
      captureLog(build.stderr, path.join(reportRoot, "build.stderr.log"), redact);
      child = build;
      if (build.pid) ownedGroups.add(build.pid);
      const result = await processExit(build, 1800000, EXIT.build);
      child = null;
      if (result.code !== 0) {
        report.buildFailure = { processExit: result, stdoutTail: fs.readFileSync(path.join(reportRoot, "build.stdout.log"), "utf8").slice(-6000), stderrTail: fs.readFileSync(path.join(reportRoot, "build.stderr.log"), "utf8").slice(-6000) };
        writeJson(reportFile, report, redact);
        throw failure("package:mac failed; inspect the private build logs.", EXIT.build);
      }
      const after = sourceFingerprint(repoRoot);
      if (after.sha256 !== fingerprint.sha256) {
        report.sourceChanged = { before: fingerprint, after, changedPaths: changedInputs(fingerprint, after) };
        writeJson(reportFile, report, redact);
        throw failure("Build inputs changed while packaging; rebuild the final source before onboarding.", EXIT.build);
      }
      const bundle = verifyBundle(bundleRoot, reportRoot);
      writeJson(buildProofFile, { schema_version: 1, source: after, bundleRoot, executableSha256: bundle.executableSha256, runtimeSha256: bundle.runtimeSha256, resourcesSha256: bundle.resourcesSha256, builtAt: new Date().toISOString(), buildReport: reportRoot });
      console.log(`Verified fresh build and signing proof: ${buildProofFile}`);
    }
    report.boundary = "package";
    writeJson(reportFile, report, redact);
    if (!fs.existsSync(buildProofFile)) throw failure("No successful harness build proof exists. Run without --no-build.", EXIT.package);
    const proof = JSON.parse(fs.readFileSync(buildProofFile, "utf8"));
    if (proof.source?.sha256 !== fingerprint.sha256) {
      report.sourceChanged = { before: proof.source, after: fingerprint, changedPaths: changedInputs(proof.source || {}, fingerprint) };
      writeJson(reportFile, report, redact);
    }
    const bundle = verifyBundle(bundleRoot, reportRoot);
    if (proof.schema_version !== 1 || proof.source.sha256 !== fingerprint.sha256 || proof.bundleRoot !== bundleRoot || proof.executableSha256 !== bundle.executableSha256 || proof.runtimeSha256 !== bundle.runtimeSha256 || proof.resourcesSha256 !== bundle.resourcesSha256) throw failure("The harness build proof does not match this source and packaged app.", EXIT.package);
    report.build = proof;
    isolatedRoot = fs.mkdtempSync(path.join(fs.realpathSync("/tmp"), "octopus-onb-"));
    fs.chmodSync(isolatedRoot, 0o700);
    const home = path.join(isolatedRoot, "home");
    const codexHome = path.join(isolatedRoot, "codex");
    const systemHome = path.join(isolatedRoot, "user");
    const temporary = path.join(isolatedRoot, "tmp");
    for (const directory of [home, codexHome, systemHome, temporary]) fs.mkdirSync(directory, { mode: 0o700 });
    if (Buffer.byteLength(path.join(home, "app-server-control", "0".repeat(24) + ".sock")) > 103) throw failure("The fresh Home path exceeds the macOS socket limit.", EXIT.launch);
    if (credential) fs.writeFileSync(path.join(codexHome, "auth.json"), credential.bytes, { mode: 0o600, flag: "wx" });
    report.isolation = { root: isolatedRoot, home, codexHome, systemHome, homeEntriesBeforeLaunch: fs.readdirSync(home), codexEntriesBeforeLaunch: fs.readdirSync(codexHome), reusedCredentialOnly: Boolean(credential), runtimeOverrides: false, preinstalledPackages: false };
    report.boundary = "launch";
    writeJson(reportFile, report, redact);
    const env = {
      HOME: systemHome, CFFIXED_USER_HOME: systemHome, ZDOTDIR: systemHome, SHELL: "/bin/zsh",
      PATH: [path.join(bundle.appRoot, "bin"), "/usr/bin", "/bin", "/usr/sbin", "/sbin", "/opt/homebrew/bin"].join(path.delimiter),
      TMPDIR: temporary + path.sep, XDG_CONFIG_HOME: path.join(systemHome, ".config"), XDG_CACHE_HOME: path.join(systemHome, ".cache"),
      LANG: process.env.LANG || "en_US.UTF-8", CODEX_HOME: codexHome, OCTOPUS_HOME: home,
      OCTOPUS_CLIENT_ACCESS_PATH: path.join(isolatedRoot, "client-access.json"),
    };
    child = spawn(bundle.executable, ["--remote-debugging-address=127.0.0.1", "--remote-debugging-port=0"], { cwd: systemHome, env, detached: true, stdio: ["ignore", "pipe", "pipe"] });
    if (child.pid) ownedGroups.add(child.pid);
    captureLog(child.stdout, path.join(reportRoot, "app.stdout.log"), redact);
    captureLog(child.stderr, path.join(reportRoot, "app.stderr.log"), redact);
    const exited = processExit(child, options.timeoutMs + 60000).catch(() => ({ code: null }));
    const activePort = path.join(home, "client/DevToolsActivePort");
    const deadline = Date.now() + 90000;
    while (!fs.existsSync(activePort)) {
      if (stopped(child) || Date.now() > deadline || interrupted) throw failure("The packaged app did not expose its isolated debugger; inspect app.stderr.log.", interrupted ? EXIT.interrupted : EXIT.launch);
      await sleep(200);
    }
    const [port, browserPath] = fs.readFileSync(activePort, "utf8").trim().split("\n");
    if (!/^\d+$/.test(port) || !browserPath?.startsWith("/devtools/browser/")) throw failure("The owned app debugger endpoint is malformed.", EXIT.launch);
    browserUrl = `ws://127.0.0.1:${port}${browserPath}`;
    const manifest = { schema_version: 1, repoRoot, bundleRoot, appRoot: bundle.appRoot, appPid: child.pid, endpoint: `http://127.0.0.1:${port}`, browserPath, pageUrl: pathToFileURL(path.join(bundle.appRoot, "dist/index.html")).href, reportFile, reportRoot, home, codexHome, mode: options.mode, target: options.target, timeoutMs: options.timeoutMs };
    const manifestFile = path.join(reportRoot, "launch.json");
    writeJson(manifestFile, manifest);
    walker = spawn(process.execPath, [path.join(__dirname, "walk-onboarding.cjs"), "--manifest", manifestFile], { cwd: systemHome, env: { ...env, OCTOPUS_ONBOARDING_REDACTION_FILE: credential ? path.join(codexHome, "auth.json") : "" }, stdio: ["ignore", "pipe", "pipe"] });
    captureLog(walker.stdout, path.join(reportRoot, "walk.stdout.log"), redact, (line) => process.stdout.write(line));
    captureLog(walker.stderr, path.join(reportRoot, "walk.stderr.log"), redact);
    const outcome = await processExit(walker, options.timeoutMs + 60000);
    walker = null;
    report = JSON.parse(fs.readFileSync(reportFile, "utf8"));
    if (outcome.code !== 0 || report.status !== "passed" || report.boundary !== options.target) throw failure(report.error || "The UI walk did not prove its requested boundary.", report.exitCode || outcome.code || EXIT.launch);
    const closer = await Cdp.connect(browserUrl);
    try { await closer.send("Browser.close"); } catch (error) { report.closeResponse = redact(error.message); } finally { closer.close(); }
    await Promise.race([exited, sleep(12000)]);
    process.exitCode = 0;
  } catch (error) {
    report = JSON.parse(fs.readFileSync(reportFile, "utf8"));
    report.status = "failed";
    report.error = redact(error.safeMessage || error.message || String(error));
    report.exitCode = interrupted ? EXIT.interrupted : error.exitCode || EXIT.launch;
    process.exitCode = report.exitCode;
    console.error(`${report.boundary}: ${report.error}\nEvidence: ${reportFile}`);
  } finally {
    walker?.kill("SIGTERM");
    if (child && !stopped(child)) {
      if (browserUrl && !interrupted) {
        try {
          const closer = await Cdp.connect(browserUrl);
          try { await closer.send("Browser.close"); } finally { closer.close(); }
          await sleep(1500);
        } catch (error) { report.closeResponse = redact(error.message); }
      }
      child.kill("SIGTERM");
      await sleep(1500);
      if (!stopped(child)) {
        try { process.kill(-child.pid, "SIGKILL"); } catch (error) { if (error.code !== "ESRCH") throw error; }
        await Promise.race([processExit(child, 3000).catch(() => null), sleep(3000)]);
      }
    }
    for (const group of ownedGroups) {
      try { process.kill(-group, "SIGTERM"); } catch (error) { if (error.code !== "ESRCH") throw error; }
    }
    if (isolatedRoot) {
      const authCopy = path.join(isolatedRoot, "codex/auth.json");
      fs.rmSync(authCopy, { force: true });
      report.credentialCopyRemoved = !fs.existsSync(authCopy);
      report.appExited = Boolean(child && stopped(child));
    }
    report.finishedAt = new Date().toISOString();
    writeJson(reportFile, report, redact);
    process.removeListener("SIGINT", stop);
    process.removeListener("SIGTERM", stop);
  }
}

class Cdp {
  constructor(socket) {
    this.socket = socket;
    this.nextId = 1;
    this.pending = new Map();
    this.events = [];
    socket.on("message", (data) => {
      const message = JSON.parse(String(data));
      const waiter = this.pending.get(message.id);
      if (!waiter) {
        if (message.method === "Runtime.exceptionThrown" && this.events.length < 64) this.events.push(message.params.exceptionDetails);
        return;
      }
      this.pending.delete(message.id);
      clearTimeout(waiter.timer);
      if (message.error) waiter.reject(new Error(message.error.message));
      else waiter.resolve(message.result);
    });
    socket.on("close", () => { for (const waiter of this.pending.values()) { clearTimeout(waiter.timer); waiter.reject(new Error("Owned debugger connection closed.")); } this.pending.clear(); });
  }
  static async connect(url) {
    const socket = new WebSocket(url, { handshakeTimeout: 30000 });
    await new Promise((resolve, reject) => { socket.once("open", resolve); socket.once("error", reject); });
    return new Cdp(socket);
  }
  send(method, params = {}) {
    return new Promise((resolve, reject) => {
      const id = this.nextId++;
      const timer = setTimeout(() => { this.pending.delete(id); reject(new Error(`Debugger call timed out: ${method}`)); }, 30000);
      this.pending.set(id, { resolve, reject, timer });
      this.socket.send(JSON.stringify({ id, method, params }));
    });
  }
  async evaluate(expression) {
    const result = await this.send("Runtime.evaluate", { expression, awaitPromise: true, returnByValue: true });
    if (result.exceptionDetails) throw new Error(result.exceptionDetails.exception?.description || result.exceptionDetails.text);
    return result.result.value;
  }
  close() { this.socket.close(); }
}

module.exports = { Cdp, EXIT, createRedactor, failure, parseArguments, readSelectedAuth, runFreshOnboarding, sleep, writeJson };
