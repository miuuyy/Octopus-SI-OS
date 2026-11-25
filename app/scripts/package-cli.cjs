const fs = require("node:fs");
const path = require("node:path");
const crypto = require("node:crypto");
const { copyTree, copyFile, deduplicateRegularFiles } = require("./package-copy.cjs");
const { createRuntimePackager } = require("./runtime-package.cjs");
const { SUPPORTED_TARGETS, resolveCliTarget, takeTargetArgument } = require("./cli-target.cjs");

function createPublicStageDir(parent, prefix) {
  const stage = fs.mkdtempSync(path.join(parent, prefix));
  fs.chmodSync(stage, 0o755);
  return stage;
}

function packageCliRuntime({ appRoot = path.resolve(__dirname, ".."), target = null, outputDir = process.env.OCTOPUS_CLI_BUILD_DIR || null } = {}) {
  const cli = resolveCliTarget({ target });
  appRoot = fs.realpathSync(appRoot);
  const repoRoot = path.dirname(appRoot);
  const outputParent = outputDir ? path.resolve(outputDir) : path.join(appRoot, "build", "cli");
  const outputRoot = path.join(outputParent, "runtime");
  fs.mkdirSync(outputParent, { recursive: true });
  const stage = createPublicStageDir(outputParent, ".runtime-stage-");
  let previous = null;
  try {
    if (cli.components.client_filesystem) {
      const clientFilesystem = require("../native/build.cjs").buildClientFilesystem();
      fs.mkdirSync(path.join(stage, "native", "build"), { recursive: true });
      copyFile(clientFilesystem, path.join(stage, "native", "build", path.basename(clientFilesystem)));
    }
    const packager = createRuntimePackager({ repoRoot, appRoot, outputRoot: stage, platform: cli.platform });
    const source = path.join(repoRoot, "registry");
    const baseline = path.join(stage, "registry");
    const library = path.join(stage, "local-packages");
    const packages = path.join(library, "registry");
    const distribution = path.join(appRoot, "distribution");
    const binaries = packager.collectBundledBinaries(source);
    for (const binary of binaries) {
      const file = path.join(repoRoot, "target", "release", binary);
      const stat = fs.lstatSync(file);
      if (!stat.isFile() || !(stat.mode & 0o111)) throw new Error(`Missing release executable: ${file}`);
    }
    packager.copyRegistryBaseline(source, baseline, path.join(distribution, "system-baseline.json"));
    packager.copyRegistrySurface(source, packages);
    packager.copyLocalPackageDefinitions(path.join(distribution, "local-packages"), library);
    require("esbuild").buildSync({
      entryPoints: [path.join(appRoot, "electron/clientDoctor.cjs")],
      outfile: path.join(stage, "client-doctor.cjs"),
      bundle: true, platform: "node", target: "node24", format: "cjs",
    });
    copyTree(path.join(repoRoot, "schemas"), path.join(stage, "schemas"), { recursive: true });
    copyTree(distribution, path.join(stage, "distribution"), { recursive: true });
    const ownership = packager.registryExecutableOwnership(packages);
    const ownerExecutables = new Set();
    const installationReferences = new Set();
    packager.rewriteBundledExecutablePaths(packages, ownership, ownerExecutables, installationReferences);
    packager.rewriteBundledExecutablePaths(baseline, ownership, ownerExecutables, installationReferences);
    const installationBinaries = packager.sharedBinaries(installationReferences);
    const sharedExecutables = packager.copyBundledBinaries(installationBinaries);
    packager.stripExecutables([...ownerExecutables, ...sharedExecutables]);
    const managed = require(path.join(source, "interface/world_provider_codex/packages/octopus-provider-codex/managed-cli/bootstrap.cjs"));
    const managedRoots = cli.components.managed_codex ? packager.bundleManagedCodex([baseline, packages]) : [];
    const providerSigning = packager.captureProviderSigningInputs([baseline, packages]);
    if (cli.components.code_signing) {
      const managedSigning = managed.captureSigning(managedRoots);
      packager.signClientResources(stage);
      managed.sealSigning(managedSigning);
      packager.sealProviderSigningInputs(providerSigning);
    }
    const deduplication = deduplicateRegularFiles(stage);
    packager.validateBundledLayout([baseline, packages], installationBinaries, ownerExecutables);
    const receipt = {
      schema: "octopus.cli_runtime_distribution", schema_version: 1,
      package_version: JSON.parse(fs.readFileSync(path.join(appRoot, "package.json"), "utf8")).version,
      platform: cli.platform, architecture: cli.architecture, target: cli.triple, components: cli.components,
      binaries: installationBinaries.map((binary) => {
        const bytes = fs.readFileSync(path.join(stage, "bin", binary));
        return { path: `bin/${binary}`, bytes: bytes.length, sha256: crypto.createHash("sha256").update(bytes).digest("hex") };
      }),
    };
    fs.writeFileSync(path.join(stage, "runtime-build-receipt.json"), `${JSON.stringify(receipt, null, 2)}\n`);
    if (fs.existsSync(outputRoot)) {
      if (!fs.lstatSync(outputRoot).isDirectory()) throw new Error(`Runtime output is not a plain directory: ${outputRoot}`);
      previous = fs.mkdtempSync(path.join(outputParent, ".runtime-previous-"));
      fs.rmdirSync(previous);
      fs.renameSync(outputRoot, previous);
    }
    fs.chmodSync(stage, 0o755);
    try { fs.renameSync(stage, outputRoot); }
    catch (error) { if (previous) fs.renameSync(previous, outputRoot); previous = null; throw error; }
    if (previous) fs.rmSync(previous, { recursive: true });
    return {
      outputRoot,
      target: cli.triple,
      binaryCount: installationBinaries.length,
      ownerExecutableCount: ownerExecutables.size,
      deduplication: {
        linkedFiles: deduplication.linkedFiles,
        deduplicatedFiles: deduplication.deduplicatedFiles,
        savedBytes: deduplication.savedBytes,
        deduplicatedGroups: deduplication.deduplicatedGroups,
      },
    };
  } finally {
    if (fs.existsSync(stage)) fs.rmSync(stage, { recursive: true });
  }
}

if (require.main === module) {
  try {
    const { target, rest } = takeTargetArgument(process.argv.slice(2));
    if (rest.length) throw new Error(`Usage: node app/scripts/package-cli.cjs [--target=<${SUPPORTED_TARGETS.join("|")}>] (consumes target/release binaries)`);
    console.log(JSON.stringify(packageCliRuntime({ target })));
    require("./build-space-notice.cjs").printBuildSpaceNotice();
  } catch (error) {
    console.error(`CLI runtime packaging failed: ${error.message}`);
    process.exitCode = 1;
  }
}

module.exports = { packageCliRuntime, createPublicStageDir };
