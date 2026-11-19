const path = require("node:path");
const { spawnSync } = require("node:child_process");

if (process.platform === "linux") process.exit(0);

const electron = require("electron");
const rebuild = path.join(path.dirname(require.resolve("@electron/rebuild")), "cli.js");
const result = spawnSync(electron, [rebuild, "-f", "-w", "node-pty"], {
  cwd: path.resolve(__dirname, ".."),
  env: { ...process.env, ELECTRON_RUN_AS_NODE: "1" },
  stdio: "inherit",
});

if (result.error) throw result.error;
if (result.status !== 0) process.exit(result.status ?? 1);
