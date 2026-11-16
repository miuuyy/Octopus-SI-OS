const path = require("node:path");
const fs = require("node:fs/promises");
const { build } = require("esbuild");
const { spawnSync } = require("node:child_process");
const { SUPPORTED_TARGETS, resolveCliTarget, takeTargetArgument } = require("./cli-target.cjs");

const USAGE = `Usage: node app/scripts/build-cli.cjs [--runtime] [--target=<${SUPPORTED_TARGETS.join("|")}>]`;

async function buildCli({ runtime = false, target = null } = {}) {
  const cli = resolveCliTarget({ target });
  const appRoot = path.resolve(__dirname, "..");
  const outdir = path.join(appRoot, "build", "cli");
  await fs.mkdir(outdir, { recursive: true });
  if (cli.components.client_filesystem && !runtime) require("../native/build.cjs").buildClientFilesystem();
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
    const repoRoot = path.dirname(appRoot);
    const registry = path.join(repoRoot, "registry");
    const packager = require("./runtime-package.cjs").createRuntimePackager({ repoRoot, appRoot, outputRoot: path.join(outdir, "runtime") });
    const provider = path.join(registry, "interface/world_provider_codex/packages/octopus-provider-codex");
    const bridge = path.join(provider, "opencodex-bridge");
    run("npm", ["ci"], bridge);
    run("npm", ["run", "build"], bridge);
    if (cli.components.desktop_native) {
      const desktop = path.join(registry, "worker_environment_kind/desktop_session/packages/octopus-system-gui-env/native");
      require(path.join(desktop, "build.cjs")).buildDesktopNative(path.join(desktop, "bin/octopus-desktop-native"));
    }
    const bins = [...new Set([...packager.collectBundledBinaries(registry), "octopus-provider-opencodex-life", "octopus-input-intake-life"])];
    run("cargo", ["build", "--locked", "--release", ...bins.flatMap((bin) => ["--bin", bin])], repoRoot);
    packager.materializeProviderLife(provider, "octopus-provider-opencodex-life");
    packager.materializeProviderLife(path.join(provider, "intake"), "octopus-input-intake-life");
    packager.validateOpenCodexOwnerAssets(provider);
    require("./package-cli.cjs").packageCliRuntime({ appRoot, target: cli.triple });
  }
  return outdir;
}

function run(command, args, cwd) {
  const result = spawnSync(command, args, { cwd, stdio: "inherit" });
  if (result.error) throw result.error;
  if (result.status !== 0) throw new Error(`${command} failed (${result.status ?? result.signal}).`);
}

if (require.main === module) Promise.resolve().then(() => {
  const { target, rest } = takeTargetArgument(process.argv.slice(2));
  if (rest.some((arg) => arg !== "--runtime")) throw new Error(USAGE);
  return buildCli({ runtime: rest.includes("--runtime"), target });
}).then((directory) => {
  process.stdout.write(`Octopus CLI client modules built in ${directory}\n`);
  require("./build-space-notice.cjs").printBuildSpaceNotice();
}).catch((error) => {
  process.stderr.write(`${error.message}\n`);
  process.exitCode = 1;
});

module.exports = { buildCli };
