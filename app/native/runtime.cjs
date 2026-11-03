const fs = require("node:fs");
const path = require("node:path");
const crypto = require("node:crypto");
const { spawnSync } = require("node:child_process");

const appRoot = path.resolve(__dirname, "..");
const electronRoot = path.join(appRoot, "node_modules", "electron");
const exactVersion = /^\d+\.\d+\.\d+$/;
const digest = /^[a-f0-9]{64}$/;
const sha256 = (bytes) => crypto.createHash("sha256").update(bytes).digest("hex");

function clientRuntime() {
  const pin = JSON.parse(fs.readFileSync(path.join(__dirname, "runtime.json"), "utf8"));
  const archives = pin?.electron_archive_sha256;
  if (pin?.schema_version !== 1 || !exactVersion.test(pin.electron) || !exactVersion.test(pin.node) || !digest.test(pin.node_headers_sha256 ?? "")
    || !archives || Object.keys(archives).sort().join(",") !== "darwin-arm64,darwin-x64" || !Object.values(archives).every((value) => digest.test(value))) {
    throw new Error("app/native/runtime.json is not a valid client runtime pin");
  }
  const declared = JSON.parse(fs.readFileSync(path.join(appRoot, "package.json"), "utf8")).devDependencies?.electron;
  if (declared !== pin.electron) throw new Error(`app/package.json must pin electron exactly to ${pin.electron}, the client runtime in app/native/runtime.json`);
  return pin;
}

function verifyNodeHeaders(headers) {
  const { electron, node } = clientRuntime();
  if (typeof headers !== "string" || !path.isAbsolute(headers)) {
    throw new Error(`Set OCTOPUS_NATIVE_NODE_HEADERS to the include/node directory of the Node ${node} headers; node app/native/runtime.cjs headers fetches them`);
  }
  const directory = fs.realpathSync(headers);
  const version = fs.readFileSync(path.join(directory, "node_version.h"), "utf8");
  const [major, minor, patch] = node.split(".");
  for (const [field, value] of [["MAJOR", major], ["MINOR", minor], ["PATCH", patch]]) {
    if (!new RegExp(`^#define NODE_${field}_VERSION\\s+${value}\\s*$`, "m").test(version)) throw new Error(`Native headers must be exactly Node ${node}, the Node of the pinned Electron ${electron}`);
  }
  return directory;
}

async function fetchNodeHeaders(buildRoot = path.join(appRoot, "build")) {
  const { node, node_headers_sha256: expected } = clientRuntime();
  const name = `node-v${node}-headers.tar.gz`;
  fs.mkdirSync(buildRoot, { recursive: true });
  const archive = path.join(buildRoot, name);
  let bytes = fs.existsSync(archive) ? fs.readFileSync(archive) : null;
  if (!bytes || sha256(bytes) !== expected) {
    const response = await fetch(`https://nodejs.org/download/release/v${node}/${name}`);
    if (!response.ok) throw new Error(`Node ${node} headers download failed: HTTP ${response.status}`);
    bytes = Buffer.from(await response.arrayBuffer());
    if (sha256(bytes) !== expected) throw new Error(`Node ${node} headers differ from the sha256 pinned in app/native/runtime.json`);
    const temporary = `${archive}.${crypto.randomBytes(6).toString("hex")}.tmp`;
    fs.writeFileSync(temporary, bytes, { flag: "wx" });
    fs.renameSync(temporary, archive);
  }
  const stage = fs.mkdtempSync(path.join(buildRoot, ".node-headers-"));
  try {
    const extracted = spawnSync("tar", ["-xzf", "-", "-C", stage], { input: bytes, encoding: "utf8", maxBuffer: 1024 * 1024 });
    if (extracted.error) throw extracted.error;
    if (extracted.status !== 0) throw new Error(`Node ${node} headers could not be extracted: ${extracted.stderr.trim() || extracted.status}`);
    verifyNodeHeaders(path.join(stage, `node-v${node}`, "include", "node"));
    const root = path.join(buildRoot, `node-v${node}`);
    fs.rmSync(root, { recursive: true, force: true });
    fs.renameSync(path.join(stage, `node-v${node}`), root);
    return verifyNodeHeaders(path.join(root, "include", "node"));
  } finally { fs.rmSync(stage, { recursive: true, force: true }); }
}

function fetchElectron(architecture = process.arch) {
  if (process.platform !== "darwin") throw new Error("The client Electron runtime is pinned for macOS");
  const { electron, node, electron_archive_sha256: archives } = clientRuntime();
  if (!Object.hasOwn(archives, `darwin-${architecture}`)) throw new Error(`The client runtime pins no Electron archive for darwin-${architecture}`);
  const installed = JSON.parse(fs.readFileSync(path.join(electronRoot, "package.json"), "utf8")).version;
  if (installed !== electron) throw new Error(`app/node_modules/electron is ${installed}, but the client runtime pins Electron ${electron}; run npm ci in app`);
  const archive = `electron-v${electron}-darwin-${architecture}.zip`;
  const checksums = JSON.parse(fs.readFileSync(path.join(electronRoot, "checksums.json"), "utf8"));
  if (checksums[archive] !== archives[`darwin-${architecture}`]) throw new Error(`Electron's checksums.json disagrees with the sha256 of ${archive} pinned in app/native/runtime.json`);
  const environment = { ...process.env, ELECTRON_INSTALL_PLATFORM: "darwin", ELECTRON_INSTALL_ARCH: architecture, npm_config_arch: architecture };
  for (const key of ["electron_use_remote_checksums", "npm_config_electron_use_remote_checksums", "ELECTRON_OVERRIDE_DIST_PATH"]) delete environment[key];
  const install = spawnSync(process.execPath, [path.join(electronRoot, "install.js")], { cwd: electronRoot, env: environment, stdio: ["ignore", 2, 2] });
  if (install.error) throw install.error;
  if (install.status !== 0) throw new Error(`Electron ${electron} download failed (${install.status ?? install.signal})`);
  const bundlePath = path.join(electronRoot, "dist", "Electron.app");
  const probe = spawnSync(path.join(bundlePath, "Contents", "MacOS", "Electron"), ["-e", "process.stdout.write(JSON.stringify({ electron: process.versions.electron, node: process.versions.node, arch: process.arch }))"], {
    env: { PATH: process.env.PATH ?? "", HOME: process.env.HOME ?? "", ELECTRON_RUN_AS_NODE: "1" }, encoding: "utf8", timeout: 60000,
  });
  if (probe.error) throw probe.error;
  let versions = null;
  try { versions = probe.status === 0 ? JSON.parse(probe.stdout) : null; } catch { versions = null; }
  if (versions?.electron !== electron || versions.node !== node || versions.arch !== architecture) {
    throw new Error(`app/node_modules/electron/dist is not Electron ${electron} (Node ${node}, ${architecture}); remove app/node_modules/electron/dist and fetch it again`);
  }
  return bundlePath;
}

if (require.main === module) {
  const [mode, ...rest] = process.argv.slice(2);
  Promise.resolve().then(() => {
    if (mode === "headers" && rest.length <= 1) return fetchNodeHeaders(rest[0] ? path.resolve(rest[0]) : undefined);
    if (mode === "electron" && !rest.length) return fetchElectron();
    throw new Error("Usage: runtime.cjs headers [build-root] | electron");
  }).then((result) => process.stdout.write(`${result}\n`), (error) => {
    process.stderr.write(`${error.message}\n`);
    process.exitCode = 1;
  });
}

module.exports = { clientRuntime, verifyNodeHeaders, fetchNodeHeaders, fetchElectron };
