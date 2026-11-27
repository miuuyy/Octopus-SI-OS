const fs = require("node:fs");
const net = require("node:net");
const path = require("node:path");
const { spawn, spawnSync } = require("node:child_process");

const appRoot = path.resolve(__dirname, "..");

function assertDevPrerequisites() {
  let sqlite = null;
  try {
    sqlite = require("node:sqlite");
  } catch (error) {
    throw new Error(
      `Octopus client dev requires Node.js with built-in node:sqlite (Node 24 LTS; current: ${process.versions.node}): ${error instanceof Error ? error.message : String(error)}`,
    );
  }
  if (typeof sqlite?.DatabaseSync !== "function") {
    throw new Error(`Octopus client dev requires Node.js with node:sqlite DatabaseSync (Node 24 LTS; current: ${process.versions.node}).`);
  }
  const manifest = JSON.parse(fs.readFileSync(path.join(appRoot, "package.json"), "utf8"));
  const missing = Object.keys(manifest.dependencies ?? {}).filter(
    (name) => !fs.existsSync(path.join(appRoot, "node_modules", name, "package.json")),
  );
  if (missing.length > 0) {
    throw new Error(`Octopus client dev cannot start with missing packages (${missing.join(", ")}); run \`npm ci --prefix app\` first.`);
  }
}

assertDevPrerequisites();
const sync = spawnSync(process.execPath, [path.join(__dirname, "sync-gesture-assets.cjs")], {
  cwd: appRoot,
  stdio: "inherit",
});
if (sync.error) throw sync.error;
if (sync.status !== 0) process.exit(sync.status ?? 1);

const vite = spawn(process.execPath, [path.join(appRoot, "node_modules", "vite", "bin", "vite.js"), "--host", "127.0.0.1"], {
  cwd: appRoot,
  env: process.env,
  stdio: "inherit",
});
let electron = null;
let stopping = false;
let forceTimer = null;

vite.once("error", fail);
vite.once("exit", (code) => {
  if (!stopping && code !== 0) fail(new Error(`Vite exited with status ${code}`));
});

void waitForPort(5174, 30_000).then(() => {
  if (stopping) return;
  electron = spawn(require("electron"), [appRoot], {
    cwd: appRoot,
    env: { ...process.env, VITE_DEV_SERVER_URL: "http://127.0.0.1:5174" },
    stdio: ["inherit", "inherit", "inherit", "ipc"],
  });
  electron.once("error", fail);
  electron.once("exit", (code) => {
    if (forceTimer) clearTimeout(forceTimer);
    forceTimer = null;
    if (!stopping) shutdown(code ?? 0);
  });
}).catch(fail);

for (const signal of ["SIGINT", "SIGTERM"]) {
  process.once(signal, () => shutdown(0, signal));
}

function waitForPort(port, timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  return new Promise((resolve, reject) => {
    const probe = () => {
      const socket = net.createConnection({ host: "127.0.0.1", port });
      let settled = false;
      const done = (ready) => {
        if (settled) return;
        settled = true;
        socket.destroy();
        if (ready) return resolve();
        if (Date.now() >= deadline) return reject(new Error(`Vite did not listen on port ${port} within ${timeoutMs}ms`));
        setTimeout(probe, 100);
      };
      socket.setTimeout(200, () => done(false));
      socket.once("connect", () => done(true));
      socket.once("error", () => done(false));
    };
    probe();
  });
}

function fail(error) {
  console.error(error instanceof Error ? error.message : String(error));
  shutdown(1);
}

function shutdown(code, signal) {
  if (stopping) return;
  stopping = true;
  if (electron && electron.exitCode == null) {
    electron.send({ type: "octopus-dev-quit" });
    forceTimer = setTimeout(() => {
      if (electron?.exitCode == null) electron.kill(signal || "SIGTERM");
    }, 12_000);
  }
  if (vite.exitCode == null) vite.kill(signal || "SIGTERM");
  process.exitCode = code;
}
