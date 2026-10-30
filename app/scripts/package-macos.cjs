const fs = require("node:fs");
const crypto = require("node:crypto");
const path = require("node:path");
const { spawnSync } = require("node:child_process");
const { copyTree, copyFile } = require("./copy-macos.cjs");
const { publishClientPresentation, sealClientPresentation } = require("./client-presentation-artifact.cjs");
const { readGeneratedSourcePaths, assertDeclaredGeneratedOutput } = require("./generated-source-boundary.cjs");

const appRoot = path.resolve(__dirname, "..");
const repoRoot = path.resolve(appRoot, "..");
const generatedOutputs = readGeneratedSourcePaths(repoRoot);
const packageVersion = JSON.parse(fs.readFileSync(path.join(appRoot, "package.json"), "utf8")).version;
if (!/^\d+\.\d+\.\d+$/.test(packageVersion)) {
  throw new Error(`package version must be a three-part numeric version: ${packageVersion}`);
}
const registryRoot = path.join(repoRoot, "registry");
const distributionRoot = path.join(appRoot, "distribution");
const baselineManifestPath = path.join(distributionRoot, "system-baseline.json");
const buildRoot = path.join(appRoot, "build");
const bundleRoot = path.join(buildRoot, "Octopus.app");
const minimumSystemVersion = "14.0";
const ownedEngineVerifier = require(path.join(registryRoot, "worker_environment_kind", "browser_session", "packages", "octopus-browser-session-env", "native", "macos", "engine-artifact.cjs"));
const browserEnginePackage = require(path.join(registryRoot, "worker_environment_kind", "browser_session", "packages", "octopus-browser-session-env", "native", "engine-mode.cjs"));
const browserEngineMode = browserEnginePackage.packagingMode(process.argv.slice(2), process.env);
const ownedEngineReceiptPath = process.env.OCTOPUS_OWNED_ELECTRON_RECEIPT;
const ownedEngine = browserEngineMode === "owned"
  ? ownedEngineVerifier.verifyEngineArtifact(ownedEngineReceiptPath, process.arch)
  : null;
const electronBundle = ownedEngine ? ownedEngine.bundlePath : path.join(appRoot, "node_modules", "electron", "dist", "Electron.app");
const browserElectronVersion = ownedEngine ? ownedEngine.source.electron_version : JSON.parse(fs.readFileSync(path.join(appRoot, "node_modules", "electron", "package.json"), "utf8")).version;
console.log(`Packaging browser engine: ${browserEngineMode} (${browserEnginePackage.engineInfo(browserEngineMode).presentationProtocol}).`);
const resourcesRoot = path.join(bundleRoot, "Contents", "Resources");
const bundledAppRoot = path.join(resourcesRoot, "app");
const macosRoot = path.join(bundleRoot, "Contents", "MacOS");
const plistPath = path.join(bundleRoot, "Contents", "Info.plist");
const icnsPath = path.join(appRoot, "public", "octopus.icns");
const openCodexOwnerRoot = path.join(
  registryRoot,
  "interface",
  "world_provider_codex",
  "packages",
  "octopus-provider-codex",
);
const openCodexBridgeRoot = path.join(openCodexOwnerRoot, "opencodex-bridge");
const managedCodexRoot = path.join(openCodexOwnerRoot, "managed-cli");
const managedCodexDistribution = require(path.join(managedCodexRoot, "bootstrap.cjs"));
const openCodexLifeBinary = "octopus-provider-opencodex-life";
const inputIntakeLifeBinary = "octopus-input-intake-life";
const { ownedProviderLives, buildOwnedNpmPackages, captureProviderSigningInputs, sealProviderSigningInputs, signClientResources, bundleManagedCodex, copyRegistrySurface, copyLocalPackageDefinitions, copyRegistryBaseline, collectBundledBinaries, sharedBinaries, materializeProviderLife, validateOpenCodexOwnerAssets, copyBundledBinaries, registryExecutableOwnership, rewriteBundledExecutablePaths, stripExecutables, transformExecutableCopies, validateBundledLayout } =
  require("./runtime-package.cjs").createRuntimePackager({ repoRoot, appRoot, outputRoot: bundledAppRoot });

for (const asset of [
  path.join(appRoot, "public", "favicon.png"),
  path.join(appRoot, "public", "octopus-app-icon.png"),
  path.join(appRoot, "public", "octopus-dev-dock-icon.png"),
  icnsPath,
]) {
  if (!fs.existsSync(asset)) throw new Error(`packaging asset is missing: ${asset}`);
}
const revision = spawnSync("git", ["rev-parse", "HEAD"], { cwd: repoRoot, encoding: "utf8" });
const sourceStatus = spawnSync("git", ["status", "--porcelain", "--untracked-files=normal"], { cwd: repoRoot, encoding: "utf8" });
run("npm", ["run", "build"], appRoot);
buildOpenCodexBridge();
buildOwnedNpmPackages();
const desktopNativeRoot = path.join(registryRoot, "worker_environment_kind", "desktop_session", "packages", "octopus-system-gui-env", "native");
const desktopNativeExecutable = path.join(desktopNativeRoot, "bin", "octopus-desktop-native");
assertDeclaredGeneratedOutput(desktopNativeExecutable, repoRoot, generatedOutputs);
require(path.join(desktopNativeRoot, "build.cjs")).buildDesktopNative(desktopNativeExecutable);
const pipBuilder = require(path.join(desktopNativeRoot, "pip", "build.cjs"));
const pipHeaders = process.env.OCTOPUS_NATIVE_NODE_HEADERS;
if (!pipHeaders) throw new Error("Packaging native PiP requires explicit OCTOPUS_NATIVE_NODE_HEADERS for Node24.15.0.");
const clientFilesystem = require("../native/build.cjs").buildClientFilesystem({ headers: pipHeaders });
const pipCompiler = fs.realpathSync(capture("xcrun", ["--find", "clang++"]));
const pipSwiftCompiler = fs.realpathSync(capture("xcrun", ["--find", "swiftc"]));
const pipSdk = fs.realpathSync(capture("xcrun", ["--sdk", "macosx", "--show-sdk-path"]));
const pipOutput = fs.realpathSync(fs.mkdtempSync(path.join(require("node:os").tmpdir(), "octopus-package-pip-")));
process.once("exit", () => fs.rmSync(pipOutput, { recursive: true, force: true }));
const pipReceipt = pipBuilder.build({ compiler: pipCompiler, swiftCompiler: pipSwiftCompiler, sdk: pipSdk, headers: fs.realpathSync(pipHeaders),
  outputRoot: pipOutput, architecture: process.arch, target: "addon", deploymentTarget: minimumSystemVersion });
const bundledBinaries = collectBundledBinaries(registryRoot);
const ownedLives = ownedProviderLives();
buildBundledBinaries([...bundledBinaries, openCodexLifeBinary, inputIntakeLifeBinary, ...ownedLives.map((life) => life.binary)]);
materializeProviderLife(openCodexOwnerRoot, openCodexLifeBinary);
materializeProviderLife(path.join(openCodexOwnerRoot, "intake"), inputIntakeLifeBinary);
for (const life of ownedLives) materializeProviderLife(life.ownerRoot, life.binary, life.crateRoot);
validateOpenCodexOwnerAssets(openCodexOwnerRoot);

fs.rmSync(bundleRoot, { recursive: true, force: true });
fs.mkdirSync(buildRoot, { recursive: true });
copyTree(electronBundle, bundleRoot, { recursive: true, verbatimSymlinks: true });
const ownedEngineCopy = ownedEngine ? ownedEngineVerifier.beginPackagedEngine(ownedEngine, bundleRoot) : null;

const electronExecutable = path.join(macosRoot, "Electron");
const octopusExecutable = path.join(macosRoot, "Octopus");
if (fs.existsSync(electronExecutable)) {
  fs.renameSync(electronExecutable, octopusExecutable);
}

fs.rmSync(path.join(resourcesRoot, "default_app.asar"), { force: true });
fs.rmSync(bundledAppRoot, { recursive: true, force: true });
fs.mkdirSync(bundledAppRoot, { recursive: true });
publishClientPresentation({ sourceRoot: path.join(desktopNativeRoot, "pip"), buildReceipt: pipReceipt, destination: path.join(resourcesRoot, "native-presentation") });

for (const item of ["dist", "electron", "package.json", "schemas", "distribution"]) {
  copyTree(path.join(appRoot, item), path.join(bundledAppRoot, item), { recursive: true });
}
fs.writeFileSync(path.join(bundledAppRoot, "build-info.json"), JSON.stringify({
  revision: revision.status === 0 ? revision.stdout.trim() : null,
  modifiedSource: sourceStatus.status === 0 ? Boolean(sourceStatus.stdout.trim()) : null,
  builtAt: new Date().toISOString(),
}, null, 2) + "\n");
fs.mkdirSync(path.join(bundledAppRoot, "native", "build"), { recursive: true });
copyFile(clientFilesystem, path.join(bundledAppRoot, "native", "build", path.basename(clientFilesystem)));
copyTree(path.join(repoRoot, "schemas"), path.join(resourcesRoot, "schemas"), { recursive: true });
const bundledRegistryRoot = path.join(bundledAppRoot, "registry");
const localPackageRoot = path.join(bundledAppRoot, "local-packages");
const localPackageRegistryRoot = path.join(localPackageRoot, "registry");
copyRegistryBaseline(registryRoot, bundledRegistryRoot, baselineManifestPath);
copyRegistrySurface(registryRoot, localPackageRegistryRoot);
copyLocalPackageDefinitions(path.join(distributionRoot, "local-packages"), localPackageRoot);
const executableOwnership = registryExecutableOwnership(localPackageRegistryRoot);
const ownerExecutables = new Set();
const installationReferences = new Set();
rewriteBundledExecutablePaths(localPackageRegistryRoot, executableOwnership, ownerExecutables, installationReferences);
rewriteBundledExecutablePaths(bundledRegistryRoot, executableOwnership, ownerExecutables, installationReferences);
const installationBinaries = sharedBinaries(installationReferences);
const installedBinaries = copyBundledBinaries(installationBinaries);
stripExecutables([...ownerExecutables, ...installedBinaries]);
const managedCodexRoots = bundleManagedCodex([bundledRegistryRoot, localPackageRegistryRoot]);
for (const dependency of [
  "@noble/hashes",
  "ajv",
  "fast-deep-equal",
  "fast-uri",
  "json-schema-traverse",
  "node-pty",
  "require-from-string",
  "ws",
]) {
  const destination = path.join(bundledAppRoot, "node_modules", dependency);
  fs.mkdirSync(path.dirname(destination), { recursive: true });
  copyTree(path.join(appRoot, "node_modules", dependency), destination, {
    recursive: true,
    filter: (candidate) => !candidate.split(path.sep).some((part) => part === "obj.target" || part === ".deps"),
  });
}
copyFile(icnsPath, path.join(resourcesRoot, "octopus.icns"));
validateBundledLayout([bundledRegistryRoot, localPackageRegistryRoot], installationBinaries, ownerExecutables);

setPlist("CFBundleName", "Octopus");
setPlist("CFBundleDisplayName", "Octopus");
setPlist("CFBundleExecutable", "Octopus");
setPlist("CFBundleIdentifier", "com.octopus.desktop");
setPlist("CFBundleIconFile", "octopus.icns");
setPlist("CFBundleShortVersionString", packageVersion);
setPlist("CFBundleVersion", packageVersion);
setPlist("LSMinimumSystemVersion", minimumSystemVersion);
setPlist("NSMicrophoneUsageDescription", "Octopus uses the microphone for persistent voice conversations with your agent.");
setPlist("NSCameraUsageDescription", "Octopus uses the camera locally to recognize hand gestures. Frames are not recorded or sent to the runtime.");
setPlistBoolean("NSCameraUseContinuityCameraDeviceType", true);

const providerSigningInputs = captureProviderSigningInputs([bundledRegistryRoot, localPackageRegistryRoot]);
const managedCodexSigningInputs = managedCodexDistribution.captureSigning(managedCodexRoots);
if (!ownedEngine) {
  for (const entry of fs.readdirSync(path.join(bundleRoot, "Contents", "Frameworks"))) {
    const nested = path.join(bundleRoot, "Contents", "Frameworks", entry);
    if (entry.endsWith(".framework") || entry.endsWith(".app")) run("codesign", ["--force", "--deep", "--sign", "-", nested], appRoot);
  }
}
signClientResources(resourcesRoot);
managedCodexDistribution.sealSigning(managedCodexSigningInputs);
sealProviderSigningInputs(providerSigningInputs, { relativeRoot: bundleRoot, receiptPath: path.join(buildRoot, "provider-signing.json") });
sealClientPresentation(path.join(resourcesRoot, "native-presentation"));
if (ownedEngineCopy) ownedEngineVerifier.sealPackagedEngine(ownedEngineCopy);
browserEnginePackage.sealBrowserEngine({ mode: browserEngineMode, bundleRoot, architecture: process.arch, electronVersion: browserElectronVersion });
run("codesign", ["--force", "--sign", "-", bundleRoot], appRoot);
if (ownedEngineCopy) ownedEngineVerifier.verifyPackagedEngineAfterSigning(ownedEngineCopy);
run("codesign", ["--verify", "--deep", "--strict", bundleRoot], appRoot);

console.log(`Packaged ${bundleRoot}`);

function buildOpenCodexBridge() {
  run("npm", ["ci"], openCodexBridgeRoot);
  run("npm", ["run", "build"], openCodexBridgeRoot);
}

function buildBundledBinaries(binaries) {
  const args = ["build", "--release"];
  for (const binary of binaries) args.push("--bin", binary);
  run("cargo", args, repoRoot);
}

function setPlistBoolean(key, value) {
  spawnSync("/usr/libexec/PlistBuddy", ["-c", `Delete :${key}`, plistPath], { encoding: "utf8" });
  run("/usr/libexec/PlistBuddy", ["-c", `Add :${key} bool ${value ? "true" : "false"}`, plistPath], appRoot);
}

function setPlist(key, value) {
  const set = spawnSync("/usr/libexec/PlistBuddy", ["-c", `Set :${key} ${value}`, plistPath], { encoding: "utf8" });
  if (set.status === 0) return;
  run("/usr/libexec/PlistBuddy", ["-c", `Add :${key} string ${value}`, plistPath], appRoot);
}

function run(command, args, cwd) {
  const result = spawnSync(command, args, { cwd, stdio: "inherit" });
  if (result.status !== 0) {
    throw new Error(`${command} ${args.join(" ")} failed with status ${result.status}`);
  }
}

function capture(command, args) {
  const result = spawnSync(command, args, { cwd: appRoot, encoding: "utf8", maxBuffer: 1024 * 1024 });
  if (result.error || result.status !== 0) throw new Error(`Package toolchain lookup failed: ${command}`);
  return result.stdout.trim();
}
