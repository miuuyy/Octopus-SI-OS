const fs = require("node:fs");
const path = require("node:path");
const crypto = require("node:crypto");
const { spawnSync } = require("node:child_process");
const { copyTree, copyFile, deduplicateRegularFiles } = require("./package-copy.cjs");

const appRootDefault = path.resolve(__dirname, "..");
const RUNTIME_NODE_MODULE_ROOTS = Object.freeze(["@noble/hashes", "ajv", "ws"]);
const SUPPORTED_LINUX_ARCHITECTURES = new Set(["x64", "arm64"]);

function parseArguments(argv, { appRoot = appRootDefault } = {}) {
  const options = {
    cliDir: path.join(appRoot, "build", "cli"),
    output: path.join(appRoot, "build", "release-linux"),
    appRoot,
    repoRoot: path.dirname(appRoot),
    dryRun: false,
  };
  for (const argument of argv) {
    if (argument === "--dry-run") {
      options.dryRun = true;
      continue;
    }
    const [key, value] = argument.split(/=(.*)/s, 2);
    if (key === "--cli-dir" && value) options.cliDir = path.resolve(value);
    else if (key === "--output" && value) options.output = path.resolve(value);
    else throw new Error("Usage: release-linux.cjs [--cli-dir=<app/build/cli>] [--output=<directory>] [--dry-run]");
  }
  if (options.output === options.cliDir || options.output.startsWith(`${options.cliDir}${path.sep}`)) {
    throw new Error("release output must be outside the CLI build directory");
  }
  return options;
}

function sha256File(file) {
  const hash = crypto.createHash("sha256");
  const descriptor = fs.openSync(file, fs.constants.O_RDONLY | fs.constants.O_NOFOLLOW);
  const buffer = Buffer.allocUnsafe(1024 * 1024);
  try {
    for (;;) {
      const count = fs.readSync(descriptor, buffer, 0, buffer.length, null);
      if (!count) break;
      hash.update(buffer.subarray(0, count));
    }
  } finally {
    fs.closeSync(descriptor);
  }
  return hash.digest("hex");
}

function requireRegularFile(file, label) {
  const stat = fs.lstatSync(file, { throwIfNoEntry: false });
  if (!stat || !stat.isFile()) throw new Error(`${label} is missing or not a regular file: ${file}`);
  return stat;
}

function requireExecutableFile(file, label) {
  const stat = requireRegularFile(file, label);
  if (!(stat.mode & 0o111)) throw new Error(`${label} is not executable: ${file}`);
  return stat;
}

function inspectCliRuntime(cliDir, expectedVersion) {
  const shared = path.join(cliDir, "shared.cjs");
  requireRegularFile(shared, "CLI shared module");
  const runtimeDir = path.join(cliDir, "runtime");
  const receiptFile = path.join(runtimeDir, "runtime-build-receipt.json");
  requireRegularFile(receiptFile, "CLI runtime build receipt");
  const receipt = JSON.parse(fs.readFileSync(receiptFile, "utf8"));
  if (receipt.schema !== "octopus.cli_runtime_distribution" || receipt.schema_version !== 1) {
    throw new Error(`unexpected CLI runtime receipt schema: ${receipt.schema} v${receipt.schema_version}`);
  }
  if (receipt.platform !== "linux") {
    throw new Error(`Linux release requires a linux runtime receipt, got '${receipt.platform}'`);
  }
  if (!SUPPORTED_LINUX_ARCHITECTURES.has(receipt.architecture)) {
    throw new Error(`unsupported Linux architecture '${receipt.architecture}'`);
  }
  if (receipt.package_version !== expectedVersion) {
    throw new Error(`CLI runtime version '${receipt.package_version}' does not match app/package.json '${expectedVersion}'`);
  }
  requireExecutableFile(path.join(runtimeDir, "bin", "octopus"), "Linux runtime binary");
  requireExecutableFile(path.join(runtimeDir, "bin", "octopus-linux-process-authority"), "Linux process authority binary");
  requireRegularFile(path.join(runtimeDir, "registry", ".octopus-source.json"), "baseline source registry identity");
  requireRegularFile(path.join(runtimeDir, "local-packages", "registry", ".octopus-source.json"), "local package registry identity");
  for (const requiredDir of ["packages", "packs"]) {
    const dir = path.join(runtimeDir, "local-packages", requiredDir);
    if (!fs.lstatSync(dir, { throwIfNoEntry: false })?.isDirectory()) {
      throw new Error(`CLI runtime package library is missing '${requiredDir}': ${dir}`);
    }
  }
  if (!Array.isArray(receipt.binaries) || receipt.binaries.length === 0) {
    throw new Error("CLI runtime build receipt lists no binaries");
  }
  for (const entry of receipt.binaries) {
    if (typeof entry?.path !== "string" || !entry.path || path.isAbsolute(entry.path) || entry.path.split("/").includes("..")) {
      throw new Error(`CLI runtime build receipt has invalid binary path: ${JSON.stringify(entry?.path)}`);
    }
    const binaryPath = path.join(runtimeDir, ...entry.path.split("/"));
    const stat = requireExecutableFile(binaryPath, `receipt binary ${entry.path}`);
    if (stat.size !== entry.bytes) {
      throw new Error(`receipt binary ${entry.path} size ${stat.size} does not match receipt (${entry.bytes})`);
    }
    const digest = sha256File(binaryPath);
    if (digest !== entry.sha256) {
      throw new Error(`receipt binary ${entry.path} sha256 does not match receipt`);
    }
  }
  return {
    architecture: receipt.architecture,
    target: receipt.target,
    binaryCount: receipt.binaries.length,
    receipt,
  };
}

function collectRuntimeNodeModules(nodeModulesRoot, roots = RUNTIME_NODE_MODULE_ROOTS) {
  const packages = new Set();
  const visit = (name) => {
    if (packages.has(name)) return;
    const pkgDir = path.join(nodeModulesRoot, ...name.split("/"));
    const pkgJsonPath = path.join(pkgDir, "package.json");
    requireRegularFile(pkgJsonPath, `runtime node_modules package '${name}'`);
    packages.add(name);
    const pkg = JSON.parse(fs.readFileSync(pkgJsonPath, "utf8"));
    for (const dep of Object.keys(pkg.dependencies ?? {})) visit(dep);
  };
  for (const root of roots) visit(root);
  return [...packages].sort();
}

function copyDirectoryFiltered(source, destination, filter) {
  copyTree(source, destination, { recursive: true, verbatimSymlinks: true, filter }, "linux");
}

function normalizeDistributionPermissions(root) {
  const visit = (entryPath) => {
    const stat = fs.lstatSync(entryPath);
    if (stat.isSymbolicLink()) return;
    if (stat.isDirectory()) {
      fs.chmodSync(entryPath, 0o755);
      for (const name of fs.readdirSync(entryPath)) {
        visit(path.join(entryPath, name));
      }
      return;
    }
    if (stat.isFile()) {
      const executable = (stat.mode & 0o100) !== 0;
      fs.chmodSync(entryPath, executable ? 0o755 : 0o644);
    }
  };
  visit(root);
}

function tarCreateArgs(tarballPath, stageParent, stem) {
  const probe = spawnSync("tar", ["--version"], { encoding: "utf8" });
  const versionText = `${probe.stdout ?? ""}${probe.stderr ?? ""}`;
  if (/GNU tar/i.test(versionText)) {
    return ["--numeric-owner", "--owner=0", "--group=0", "-czf", tarballPath, "-C", stageParent, stem];
  }
  if (/bsdtar/i.test(versionText)) {
    return ["--numeric-owner", "--uid", "0", "--gid", "0", "--uname", "root", "--gname", "root", "-czf", tarballPath, "-C", stageParent, stem];
  }
  return ["-czf", tarballPath, "-C", stageParent, stem];
}

function writeChecksums(output, assets) {
  const lines = assets
    .map((asset) => `${sha256File(asset)}  ${path.basename(asset)}`)
    .sort((left, right) => left.slice(66).localeCompare(right.slice(66)));
  const file = path.join(output, "checksums.txt");
  fs.writeFileSync(file, `${lines.join("\n")}\n`);
  return file;
}

function run(command, args, options = {}) {
  const result = spawnSync(command, args, { encoding: "utf8", ...options });
  if (result.error) throw result.error;
  if (result.status !== 0) {
    const detail = `${result.stderr ?? ""}${result.stdout ?? ""}`.trim();
    throw new Error(`${command} ${args.join(" ")} failed with status ${result.status ?? result.signal}${detail ? `: ${detail}` : ""}`);
  }
  return result.stdout ?? "";
}

function verifyExtractedArchive(archivePath, stem, version, expectedSha256, { verifyRoot, receipt }) {
  const actualSha = sha256File(archivePath);
  if (actualSha !== expectedSha256) {
    throw new Error(`tarball digest mismatch during verification (${actualSha} != ${expectedSha256})`);
  }
  run("tar", ["-xzf", archivePath, "-C", verifyRoot]);
  const extracted = path.join(verifyRoot, stem);
  if (!fs.lstatSync(extracted, { throwIfNoEntry: false })?.isDirectory()) {
    throw new Error(`extracted archive is missing top-level directory '${stem}'`);
  }
  for (const dirRel of [[], ["app", "build", "cli", "runtime"]]) {
    const dirPath = path.join(extracted, ...dirRel);
    const dirStat = fs.lstatSync(dirPath, { throwIfNoEntry: false });
    if (!dirStat?.isDirectory()) {
      throw new Error(`extracted archive is missing required directory: ${dirPath}`);
    }
    const mode = dirStat.mode & 0o777;
    if ((mode & 0o755) !== 0o755) {
      throw new Error(`extracted directory is not world-traversable (0755): ${dirPath} (mode 0${mode.toString(8)})`);
    }
  }
  requireExecutableFile(path.join(extracted, "octopus"), "extracted octopus launcher");
  requireRegularFile(path.join(extracted, "app", "build", "cli", "shared.cjs"), "extracted shared.cjs");
  requireRegularFile(path.join(extracted, "app", "scripts", "safe-self-update.cjs"), "extracted safe-self-update.cjs");
  requireExecutableFile(path.join(extracted, "app", "build", "cli", "runtime", "bin", "octopus"), "extracted runtime binary");
  requireExecutableFile(
    path.join(extracted, "app", "build", "cli", "runtime", "bin", "octopus-linux-process-authority"),
    "extracted process authority binary",
  );
  if (Array.isArray(receipt?.binaries)) {
    const extractedRuntime = path.join(extracted, "app", "build", "cli", "runtime");
    for (const entry of receipt.binaries) {
      const binaryPath = path.join(extractedRuntime, ...entry.path.split("/"));
      const stat = requireExecutableFile(binaryPath, `extracted receipt binary ${entry.path}`);
      if (stat.size !== entry.bytes) {
        throw new Error(`extracted receipt binary ${entry.path} size ${stat.size} does not match receipt (${entry.bytes})`);
      }
      const digest = sha256File(binaryPath);
      if (digest !== entry.sha256) {
        throw new Error(`extracted receipt binary ${entry.path} sha256 does not match receipt`);
      }
    }
  }
  const versionOut = run(path.join(extracted, "octopus"), ["--version"], {
    env: { ...process.env, OCTOPUS_NODE: process.execPath },
  }).trim();
  if (versionOut !== `Octopus ${version}`) {
    throw new Error(`extracted ./octopus --version returned '${versionOut}', expected 'Octopus ${version}'`);
  }
  const probeScript = [
    "const envMod = require('./app/cli/environment.cjs');",
    "const env = envMod.resolveEnvironment({ home: process.argv[1] });",
    "envMod.requireDistribution(env);",
    "require('./app/cli/transcript.cjs');",
    "const ssu = require('./app/scripts/safe-self-update.cjs');",
    "if (typeof ssu.verifyStaging !== 'function' || typeof ssu.restartRuntime !== 'function') throw new Error('invalid safe-self-update export');",
    "console.log(JSON.stringify({ installationRoot: env.installationRoot }));",
  ].join(" ");
  run(process.execPath, ["-e", probeScript, "/tmp/octo-probe"], { cwd: extracted });
}

function populateStandaloneLinuxDir(stage, {
  appRoot = appRootDefault,
  repoRoot = path.dirname(appRoot),
  cliDir = null,
  nodeModules = null,
  copyCliRuntime = true,
} = {}) {
  const resolvedAppRoot = path.resolve(appRoot);
  const resolvedRepoRoot = path.resolve(repoRoot);
  const resolvedModules = nodeModules ?? collectRuntimeNodeModules(path.join(resolvedAppRoot, "node_modules"));
  fs.mkdirSync(stage, { recursive: true });

  copyFile(path.join(resolvedRepoRoot, "octopus"), path.join(stage, "octopus"), "linux");
  fs.chmodSync(path.join(stage, "octopus"), 0o755);
  copyFile(path.join(resolvedRepoRoot, "LICENSE"), path.join(stage, "LICENSE"), "linux");
  copyFile(path.join(resolvedRepoRoot, "README.md"), path.join(stage, "README.md"), "linux");
  copyDirectoryFiltered(path.join(resolvedRepoRoot, "schemas"), path.join(stage, "schemas"));

  fs.mkdirSync(path.join(stage, "app"), { recursive: true });
  copyFile(path.join(resolvedAppRoot, "package.json"), path.join(stage, "app", "package.json"), "linux");
  copyDirectoryFiltered(
    path.join(resolvedAppRoot, "cli"),
    path.join(stage, "app", "cli"),
    (sourcePath) => !sourcePath.endsWith(".ts"),
  );
  copyDirectoryFiltered(path.join(resolvedAppRoot, "electron"), path.join(stage, "app", "electron"));
  copyDirectoryFiltered(path.join(resolvedAppRoot, "schemas"), path.join(stage, "app", "schemas"));
  copyDirectoryFiltered(
    path.join(resolvedAppRoot, "distribution", "client-extensions"),
    path.join(stage, "app", "distribution", "client-extensions"),
  );
  const catalogRel = path.join("src", "components", "chat", "toolActivityPresentationCatalog.json");
  fs.mkdirSync(path.join(stage, "app", path.dirname(catalogRel)), { recursive: true });
  copyFile(path.join(resolvedAppRoot, catalogRel), path.join(stage, "app", catalogRel), "linux");

  fs.mkdirSync(path.join(stage, "app", "scripts"), { recursive: true });
  copyFile(
    path.join(resolvedAppRoot, "scripts", "safe-self-update.cjs"),
    path.join(stage, "app", "scripts", "safe-self-update.cjs"),
    "linux",
  );

  fs.mkdirSync(path.join(stage, "app", "build", "cli"), { recursive: true });
  if (copyCliRuntime) {
    if (!cliDir) throw new Error("populateStandaloneLinuxDir requires cliDir when copyCliRuntime is true");
    copyFile(path.join(cliDir, "shared.cjs"), path.join(stage, "app", "build", "cli", "shared.cjs"), "linux");
    copyDirectoryFiltered(path.join(cliDir, "runtime"), path.join(stage, "app", "build", "cli", "runtime"));
  }

  for (const name of resolvedModules) {
    const from = path.join(resolvedAppRoot, "node_modules", ...name.split("/"));
    const to = path.join(stage, "app", "node_modules", ...name.split("/"));
    fs.mkdirSync(path.dirname(to), { recursive: true });
    copyDirectoryFiltered(from, to);
  }

  const authoritySource = path.join(
    resolvedRepoRoot,
    "registry",
    "worker_environment_kind",
    "process_session",
    "packages",
    "octopus-linux-process-authority",
  );
  const authorityDest = path.join(stage, "linux-process-authority");
  fs.mkdirSync(authorityDest, { recursive: true });
  copyFile(path.join(authoritySource, "install-linux.sh"), path.join(authorityDest, "install-linux.sh"), "linux");
  fs.chmodSync(path.join(authorityDest, "install-linux.sh"), 0o755);
  copyFile(
    path.join(authoritySource, "octopus-process-authority.service"),
    path.join(authorityDest, "octopus-process-authority.service"),
    "linux",
  );

  normalizeDistributionPermissions(stage);
  return { nodeModules: resolvedModules };
}

function packageLinuxRelease(options = {}) {
  const appRoot = path.resolve(options.appRoot ?? appRootDefault);
  const repoRoot = path.resolve(options.repoRoot ?? path.dirname(appRoot));
  const cliDir = path.resolve(options.cliDir ?? path.join(appRoot, "build", "cli"));
  const output = path.resolve(options.output ?? path.join(appRoot, "build", "release-linux"));
  const dryRun = options.dryRun === true;

  const pkg = JSON.parse(fs.readFileSync(path.join(appRoot, "package.json"), "utf8"));
  const version = pkg.version;
  if (!/^\d+\.\d+\.\d+$/.test(version)) {
    throw new Error(`package version must be a three-part numeric version: ${version}`);
  }
  const inspected = inspectCliRuntime(cliDir, version);
  const nodeModules = collectRuntimeNodeModules(path.join(appRoot, "node_modules"));
  const stem = `Octopus-${version}-linux-${inspected.architecture}`;
  const tarballName = `${stem}.tar.gz`;
  const sha256Name = `${tarballName}.sha256`;
  const installerSource = path.join(appRoot, "release", "install-linux.sh");
  requireExecutableFile(installerSource, "Linux installer script");
  const installerPath = path.join(output, "install-linux.sh");

  if (dryRun) {
    return {
      applied: false,
      version,
      architecture: inspected.architecture,
      target: inspected.target,
      stem,
      tarball: path.join(output, tarballName),
      sha256File: path.join(output, sha256Name),
      installer: installerPath,
      checksums: path.join(output, "checksums.txt"),
      nodeModules,
      binaryCount: inspected.binaryCount,
    };
  }

  fs.rmSync(output, { recursive: true, force: true });
  fs.mkdirSync(output, { recursive: true });
  const stageParent = fs.mkdtempSync(path.join(output, ".linux-stage-"));
  const verifyRoot = fs.mkdtempSync(path.join(output, ".linux-verify-"));
  try {
    const stage = path.join(stageParent, stem);
    populateStandaloneLinuxDir(stage, {
      appRoot,
      repoRoot,
      cliDir,
      nodeModules,
      copyCliRuntime: true,
    });
    const deduplication = deduplicateRegularFiles(stage);

    const tarballPath = path.join(output, tarballName);
    run("tar", tarCreateArgs(tarballPath, stageParent, stem), {
      env: { ...process.env, COPYFILE_DISABLE: "1" },
    });

    const tarballSha256 = sha256File(tarballPath);
    const sha256Path = path.join(output, sha256Name);
    fs.writeFileSync(sha256Path, `${tarballSha256}  ${tarballName}\n`);

    verifyExtractedArchive(tarballPath, stem, version, tarballSha256, { verifyRoot, receipt: inspected.receipt });

    fs.copyFileSync(installerSource, installerPath);
    fs.chmodSync(installerPath, 0o755);

    const assets = [tarballPath, sha256Path, installerPath];
    const checksumsPath = writeChecksums(output, assets);
    return {
      applied: true,
      version,
      architecture: inspected.architecture,
      target: inspected.target,
      stem,
      tarball: tarballPath,
      tarballBytes: fs.statSync(tarballPath).size,
      tarballSha256,
      sha256File: sha256Path,
      installer: installerPath,
      checksums: checksumsPath,
      nodeModules,
      binaryCount: inspected.binaryCount,
      deduplication: {
        linkedFiles: deduplication.linkedFiles,
        deduplicatedFiles: deduplication.deduplicatedFiles,
        savedBytes: deduplication.savedBytes,
        deduplicatedGroups: deduplication.deduplicatedGroups,
      },
    };
  } finally {
    fs.rmSync(stageParent, { recursive: true, force: true });
    fs.rmSync(verifyRoot, { recursive: true, force: true });
  }
}

function main(argv) {
  const options = parseArguments(argv);
  const result = packageLinuxRelease(options);
  if (options.dryRun) {
    console.log(JSON.stringify(result, null, 2));
    return;
  }
  console.log(`${path.basename(result.tarball)}\t${result.tarballBytes}`);
  console.log(`${path.basename(result.sha256File)}\t${fs.statSync(result.sha256File).size}`);
  console.log(`${path.basename(result.installer)}\t${fs.statSync(result.installer).size}`);
  if (result.deduplication?.deduplicatedFiles > 0) {
    const savedMiB = (result.deduplication.savedBytes / (1024 * 1024)).toFixed(1);
    console.log(
      `Hardlink deduplication collapsed ${result.deduplication.deduplicatedFiles} duplicate files (${savedMiB} MiB saved across ${result.deduplication.deduplicatedGroups} groups)`,
    );
  }
  console.log(`Linux headless release assets for ${result.version} (${result.architecture}) are in ${options.output}`);
}

if (require.main === module) {
  try {
    main(process.argv.slice(2));
  } catch (error) {
    console.error(`Linux release failed: ${error.message}`);
    process.exitCode = 1;
  }
}

module.exports = {
  RUNTIME_NODE_MODULE_ROOTS,
  parseArguments,
  inspectCliRuntime,
  collectRuntimeNodeModules,
  normalizeDistributionPermissions,
  populateStandaloneLinuxDir,
  tarCreateArgs,
  packageLinuxRelease,
  sha256File,
};
