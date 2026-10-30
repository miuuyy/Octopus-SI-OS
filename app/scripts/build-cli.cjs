const path = require("node:path");
const fs = require("node:fs/promises");
const { build } = require("esbuild");
const { spawnSync } = require("node:child_process");

async function buildCli({ runtime = false } = {}) {
  const appRoot = path.resolve(__dirname, "..");
  const outdir = path.join(appRoot, "build", "cli");
  await fs.mkdir(outdir, { recursive: true });
  if (process.platform === "darwin" && !runtime) require("../native/build.cjs").buildClientFilesystem();
  await build({
    entryPoints: [path.join(appRoot, "cli", "shared.ts")],
    outfile: path.join(outdir, "shared.cjs"),
    bundle: true,
    platform: "node",
    target: "node24",
    format: "cjs",
    legalComments: "none",
    define: { "import.meta.env.DEV": "false" },
    logLevel: "warning",
  });
  if (runtime) {
    if (process.platform !== "darwin") throw new Error("The current Octopus CLI distribution is built for macOS.");
    const repoRoot = path.dirname(appRoot);
    const registry = path.join(repoRoot, "registry");
    const packager = require("./runtime-package.cjs").createRuntimePackager({ repoRoot, appRoot, outputRoot: path.join(outdir, "runtime") });
    const provider = path.join(registry, "interface/world_provider_codex/packages/octopus-provider-codex");
    const bridge = path.join(provider, "opencodex-bridge");
    run("npm", ["ci"], bridge);
    run("npm", ["run", "build"], bridge);
    packager.buildOwnedNpmPackages();
    const desktop = path.join(registry, "worker_environment_kind/desktop_session/packages/octopus-system-gui-env/native");
    require(path.join(desktop, "build.cjs")).buildDesktopNative(path.join(desktop, "bin/octopus-desktop-native"));
    const ownedLives = packager.ownedProviderLives(registry);
    const bins = [...new Set([...packager.collectBundledBinaries(registry), "octopus-provider-opencodex-life", "octopus-input-intake-life", ...ownedLives.map((life) => life.binary)])];
    run("cargo", ["build", "--locked", "--release", ...bins.flatMap((bin) => ["--bin", bin])], repoRoot);
    packager.materializeProviderLife(provider, "octopus-provider-opencodex-life");
    packager.materializeProviderLife(path.join(provider, "intake"), "octopus-input-intake-life");
    for (const life of ownedLives) packager.materializeProviderLife(life.ownerRoot, life.binary, life.crateRoot);
    packager.validateOpenCodexOwnerAssets(provider);
    require("./package-cli.cjs").packageCliRuntime({ appRoot });
  }
  return outdir;
}

function run(command, args, cwd) {
  const result = spawnSync(command, args, { cwd, stdio: "inherit" });
  if (result.error) throw result.error;
  if (result.status !== 0) throw new Error(`${command} failed (${result.status ?? result.signal}).`);
}

if (require.main === module) Promise.resolve().then(() => {
  if (process.argv.slice(2).some((arg) => arg !== "--runtime")) throw new Error("Usage: node app/scripts/build-cli.cjs [--runtime]");
  return buildCli({ runtime: process.argv.includes("--runtime") });
}).then((directory) => {
  process.stdout.write(`Octopus CLI client modules built in ${directory}\n`);
}).catch((error) => {
  process.stderr.write(`${error.message}\n`);
  process.exitCode = 1;
});

module.exports = { buildCli };
