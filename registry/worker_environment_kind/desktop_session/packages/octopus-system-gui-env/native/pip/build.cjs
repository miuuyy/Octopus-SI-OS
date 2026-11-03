'use strict';

const fs = require('node:fs');
const path = require('node:path');
const crypto = require('node:crypto');
const { spawnSync } = require('node:child_process');

const sha = bytes => crypto.createHash('sha256').update(bytes).digest('hex');
function regular(file) {
  const stat = fs.lstatSync(file);
  if (!stat.isFile() || stat.isSymbolicLink()) throw new Error(`Expected regular source file: ${file}`);
  return fs.readFileSync(file);
}
function verifySources() {
  const manifest = JSON.parse(regular(path.join(__dirname, 'manifest.json')));
  if (manifest.schema_version !== 1 || manifest.runtime_activation !== 'held' || manifest.auth_bootstrap !== 'source_implemented') {
    throw new Error('Receiver source contract differs from the reviewed native package');
  }
  for (const entry of manifest.files) {
    if (path.basename(entry.path) !== entry.path || sha(regular(path.join(__dirname, entry.path))) !== entry.sha256) {
      throw new Error(`Receiver source hash differs: ${entry.path}`);
    }
  }
  const settingsMembers = ['assets/SettingsAccessory.bundle/Contents/Info.plist', 'assets/SettingsAccessory.bundle/Contents/Resources/Assets.car'];
  if (!manifest.settings_resources || Object.keys(manifest.settings_resources).length !== settingsMembers.length) throw new Error('Settings image source inventory differs');
  for (const name of settingsMembers) {
    const bytes = regular(path.join(__dirname, name)), entry = manifest.settings_resources[name];
    if (!entry || bytes.length !== entry.bytes || sha(bytes) !== entry.sha256) throw new Error('Settings image source bytes differ');
  }
  return manifest;
}
function canonical(file, directory = false) {
  if (!path.isAbsolute(file) || fs.realpathSync(file) !== file) throw new Error('Build inputs must be canonical absolute paths');
  if (directory ? !fs.lstatSync(file).isDirectory() : !fs.lstatSync(file).isFile()) throw new Error('Build input has the wrong type');
  return file;
}
function runtimeVersion(value) { return typeof value === 'string' && /^\d+\.\d+\.\d+$/.test(value); }
function planBuild({ compiler, swiftCompiler, sdk, headers, outputRoot, architecture, target, deploymentTarget, electronVersion, nodeVersion }) {
  const manifest = verifySources();
  canonical(compiler); canonical(swiftCompiler); canonical(sdk, true); canonical(headers, true); canonical(outputRoot, true);
  if (path.basename(swiftCompiler) !== 'swift-frontend') throw new Error('The explicit Swift compiler must be the canonical swift-frontend executable');
  if (!['arm64', 'x64'].includes(architecture) || !['addon', 'spec'].includes(target)) throw new Error('Unsupported native build target');
  if (typeof deploymentTarget !== 'string' || !/^\d+\.\d+(?:\.\d+)?$/.test(deploymentTarget)) throw new Error('Native deployment target must be explicit');
  if (outputRoot.split(path.sep).some(part => part.endsWith('.app')) || outputRoot.startsWith(__dirname + path.sep) || outputRoot === __dirname) {
    throw new Error('Receiver build output must be separate from source and application bundles');
  }
  if (Number(deploymentTarget.split('.')[0]) < 13) throw new Error('Native SwiftUI presentation requires macOS 13 or later');
  if (!runtimeVersion(electronVersion) || !runtimeVersion(nodeVersion)) throw new Error('Native build requires the exact Electron and Node versions of the client runtime it targets');
  const nodeHeader = regular(path.join(headers, 'node_version.h')).toString('utf8');
  const [major, minor, patch] = nodeVersion.split('.');
  for (const [field, value] of [['MAJOR', major], ['MINOR', minor], ['PATCH', patch]]) {
    if (!new RegExp(`^#define NODE_${field}_VERSION\\s+${value}\\s*$`, 'm').test(nodeHeader)) throw new Error(`Node headers must be exactly ${nodeVersion}`);
  }
  const headerNames = ['node_api.h', 'node_api_types.h', 'js_native_api.h', 'js_native_api_types.h', 'node_version.h'];
  const stackInputs = ['stack.h', 'stack.mm', 'assets/pop-out-window-egg@3x.png', 'assets/SettingsAccessory.bundle/Contents/Info.plist', 'assets/SettingsAccessory.bundle/Contents/Resources/Assets.car'];
  const stackHashes = Object.fromEntries(stackInputs.map(name => [name, sha(regular(path.join(__dirname, name)))]));
  const asset = regular(path.join(__dirname, 'assets/pop-out-window-egg@3x.png'));
  if (asset.length !== 3767 || sha(asset) !== '8979fa7ce7b7dabf97ab3e73c5e7652cbf5564a94ed7cd213fa070eebc2519ea') {
    throw new Error('Native PiP placement asset differs from the exact authorized source');
  }
  const output = path.join(outputRoot, target === 'addon' ? manifest.module : 'octopus-pip-receiver-spec');
  if (fs.existsSync(output)) throw new Error('Native output already exists; select a fresh explicit output directory');
  const swiftObject = path.join(outputRoot, 'settings-visuals.o');
  const swiftHeader = path.join(outputRoot, 'OctopusSettingsVisuals-Swift.h');
  if (fs.existsSync(swiftObject) || fs.existsSync(swiftHeader)) throw new Error('SwiftUI output already exists; select a fresh explicit output directory');
  const swiftLibrary = canonical(path.join(path.dirname(swiftCompiler), '../lib/swift/macosx'), true);
  const swiftSDKLibrary = canonical(path.join(sdk, 'usr/lib/swift'), true);
  const swiftArgs = ['-frontend', '-swift-version', '5', '-parse-as-library', '-O',
    '-target', `${architecture === 'x64' ? 'x86_64' : 'arm64'}-apple-macosx${deploymentTarget}`, '-sdk', sdk,
    '-module-name', 'OctopusSettingsVisuals', '-emit-object', '-emit-objc-header-path', swiftHeader,
    '-module-cache-path', path.join(outputRoot, 'swift-module-cache'), path.join(__dirname, 'settings-visuals.swift'), '-o', swiftObject];
  const sources = target === 'addon' ? ['receiver.mm', 'bootstrap.mm', 'binding.mm', 'stack.mm', 'permissions.mm'] : ['receiver.mm', 'bootstrap.mm', 'receiver-spec.mm', 'stack.mm', 'permissions.mm'];
  const args = ['--driver-mode=g++', '-std=c++20', '-fobjc-arc', '-fblocks', '-fvisibility=hidden', '-DNAPI_VERSION=8',
    `-mmacosx-version-min=${deploymentTarget}`,
    '-arch', architecture === 'x64' ? 'x86_64' : 'arm64', '-isysroot', sdk, '-I', headers, '-I', outputRoot,
    '-L', swiftLibrary, '-L', swiftSDKLibrary, '-Wl,-rpath,/usr/lib/swift', '-framework', 'SwiftUI',
    '-framework', 'Foundation', '-framework', 'AppKit', '-framework', 'QuartzCore', '-framework', 'CoreGraphics',
    '-framework', 'CoreVideo', '-framework', 'CoreImage', '-framework', 'Security', '-framework', 'CoreServices',
    ...(target === 'addon' ? ['-bundle', '-undefined', 'dynamic_lookup'] : []),
    ...sources.map(file => path.join(__dirname, file)), swiftObject, '-o', output];
  return { schema_version: 2, electron_version: electronVersion, header_version: nodeVersion, source_sha256: Object.fromEntries(manifest.files.map(entry => [entry.path, entry.sha256])), compiler, compiler_sha256: sha(regular(compiler)), sdk,
    sdk_settings_sha256: sha(regular(path.join(sdk, 'SDKSettings.json'))), headers,
    header_sha256: Object.fromEntries(headerNames.map(name => [name, sha(regular(path.join(headers, name)))])),
    swift: { compiler: swiftCompiler, compiler_sha256: sha(regular(swiftCompiler)), args: swiftArgs,
      source: 'settings-visuals.swift', source_sha256: sha(regular(path.join(__dirname, 'settings-visuals.swift'))),
      object: swiftObject, header: swiftHeader, library: swiftLibrary, sdkLibrary: swiftSDKLibrary },
    stack_sha256: stackHashes,
    architecture, target, deploymentTarget, output, args, manifest_sha256: sha(regular(path.join(__dirname, 'manifest.json'))),
    nativeExecution: false, authBootstrap: manifest.auth_bootstrap, runtimeActivation: manifest.runtime_activation };
}
function build(request) {
  if (process.platform !== 'darwin') throw new Error('PiP receiver compilation requires macOS');
  const plan = planBuild(request);
  const swiftResult = spawnSync(plan.swift.compiler, plan.swift.args, { stdio: 'inherit', timeout: 600000 });
  if (swiftResult.error) throw swiftResult.error;
  if (swiftResult.status !== 0) throw new Error(`PiP SwiftUI compilation failed (${swiftResult.status})`);
  const swiftObjectHash = sha(regular(plan.swift.object)), swiftHeaderHash = sha(regular(plan.swift.header));
  const result = spawnSync(plan.compiler, plan.args, { stdio: 'inherit', timeout: 600000 });
  if (result.error) throw result.error;
  if (result.status !== 0) throw new Error(`PiP native compilation failed (${result.status})`);
  verifySources();
  if (sha(regular(plan.swift.compiler)) !== plan.swift.compiler_sha256 || sha(regular(plan.swift.object)) !== swiftObjectHash || sha(regular(plan.swift.header)) !== swiftHeaderHash ||
      sha(regular(plan.compiler)) !== plan.compiler_sha256 || sha(regular(path.join(__dirname, 'manifest.json'))) !== plan.manifest_sha256 ||
      sha(regular(path.join(plan.sdk, 'SDKSettings.json'))) !== plan.sdk_settings_sha256 ||
      Object.entries(plan.header_sha256).some(([name, digest]) => sha(regular(path.join(plan.headers, name))) !== digest) ||
      Object.entries(plan.stack_sha256).some(([name, digest]) => sha(regular(path.join(__dirname, name))) !== digest)) {
    throw new Error('Native build inputs changed during compilation');
  }
  const receipt = { ...plan, swift: { ...plan.swift, object_sha256: swiftObjectHash, header_sha256: swiftHeaderHash }, output_sha256: sha(regular(plan.output)), compiled: true, nativeExecution: false };
  fs.writeFileSync(plan.output + '.receipt.json', JSON.stringify(receipt, null, 2) + '\n', { flag: 'wx', mode: 0o600 });
  return receipt;
}
function artifactDirectory(architecture) {
  if (!['arm64', 'x64'].includes(architecture)) throw new Error('Unsupported PiP artifact architecture');
  return path.join(__dirname, 'bin', `darwin-${architecture}`);
}
function verifyReceipt(receipt, architecture) {
  const manifest = verifySources();
  if (receipt?.schema_version !== 2 || receipt.target !== 'addon' || receipt.architecture !== architecture || receipt.compiled !== true || receipt.nativeExecution !== false
      || !runtimeVersion(receipt.electron_version) || !runtimeVersion(receipt.header_version) || receipt.manifest_sha256 !== sha(regular(path.join(__dirname, 'manifest.json')))) {
    throw new Error('PiP native artifact receipt differs from its exact build/source contract');
  }
  const expected = Object.fromEntries(manifest.files.map(entry => [entry.path, entry.sha256]));
  if (JSON.stringify(receipt.source_sha256) !== JSON.stringify(expected)) throw new Error('PiP native artifact sources differ');
  if (receipt.swift?.source !== 'settings-visuals.swift' || receipt.swift.source_sha256 !== expected['settings-visuals.swift']
      || !['compiler_sha256', 'object_sha256', 'header_sha256'].every(key => /^[a-f0-9]{64}$/.test(receipt.swift[key] || ''))) throw new Error('PiP native SwiftUI source or compiler receipt differs');
  const resourceMembers = ['stack.h', 'stack.mm', 'assets/pop-out-window-egg@3x.png', 'assets/SettingsAccessory.bundle/Contents/Info.plist', 'assets/SettingsAccessory.bundle/Contents/Resources/Assets.car'];
  if (!receipt.stack_sha256 || Object.keys(receipt.stack_sha256).length !== resourceMembers.length) throw new Error('PiP native resource inventory differs');
  for (const file of resourceMembers) if (receipt.stack_sha256[file] !== sha(regular(path.join(__dirname, file)))) throw new Error('PiP native artifact asset differs');
  return manifest;
}
function publishArtifact(receipt) {
  verifyReceipt(receipt, receipt.architecture);
  if (sha(regular(canonical(receipt.output))) !== receipt.output_sha256) throw new Error('PiP compiler output differs from its receipt');
  const destination = artifactDirectory(receipt.architecture);
  fs.mkdirSync(destination, { recursive: true }); canonical(destination, true);
  const binary = path.join(destination, 'octopus-pip-receiver.node');
  const temporary = `${binary}.${crypto.randomBytes(12).toString('hex')}`;
  fs.copyFileSync(receipt.output, temporary, fs.constants.COPYFILE_EXCL);
  fs.renameSync(temporary, binary);
  writeArtifactReceipt(destination, { ...receipt, output: 'octopus-pip-receiver.node', compiled_output_sha256: receipt.output_sha256, signing: null });
  return destination;
}
function writeArtifactReceipt(directory, receipt) {
  const destination = path.join(directory, 'receipt.json'), temporary = `${destination}.${crypto.randomBytes(12).toString('hex')}`;
  fs.writeFileSync(temporary, JSON.stringify(receipt, null, 2) + '\n', { flag: 'wx', mode: 0o644 }); fs.renameSync(temporary, destination);
}
function sealArtifact(architecture) {
  const directory = canonical(artifactDirectory(architecture), true);
  const receipt = JSON.parse(regular(path.join(directory, 'receipt.json')));
  verifyReceipt(receipt, architecture);
  writeArtifactReceipt(directory, { ...receipt, output_sha256: sha(regular(path.join(directory, 'octopus-pip-receiver.node'))), signing: { scheme: 'ad-hoc', owner: 'package-macos.cjs' } });
}
function verifyArtifact({ architecture, nodeVersion, electronVersion }) {
  const directory = canonical(artifactDirectory(architecture), true);
  const receipt = JSON.parse(regular(path.join(directory, 'receipt.json')));
  const manifest = verifyReceipt(receipt, architecture);
  if (nodeVersion !== receipt.header_version || electronVersion !== receipt.electron_version || receipt.output !== manifest.module
    || receipt.signing?.scheme !== 'ad-hoc' || receipt.signing.owner !== 'package-macos.cjs' || !/^[a-f0-9]{64}$/.test(receipt.compiled_output_sha256 || '')) throw new Error('PiP native artifact runtime or package signing identity differs');
  const modulePath = canonical(path.join(directory, receipt.output));
  if (sha(regular(modulePath)) !== receipt.output_sha256) throw new Error('PiP native artifact bytes differ from their package receipt');
  return { modulePath, placementAssetPath: canonical(path.join(__dirname, 'assets/pop-out-window-egg@3x.png')), settingsAssetBundlePath: canonical(path.join(__dirname, 'assets/SettingsAccessory.bundle'), true), outputSha256: receipt.output_sha256 };
}

module.exports = { verifySources, verifyReceipt, planBuild, build, verifyArtifact, publishArtifact, sealArtifact };
if (require.main === module) {
  try {
    const [mode, requestFile, ...extra] = process.argv.slice(2);
    if (extra.length || !['verify-source', 'plan', 'build'].includes(mode) || (mode === 'verify-source' ? requestFile : !requestFile)) {
      throw new Error('Usage: build.cjs verify-source | {plan|build} /absolute/request.json');
    }
    const result = mode === 'verify-source' ? verifySources() : (mode === 'build' ? build : planBuild)(JSON.parse(regular(canonical(requestFile))));
    process.stdout.write(JSON.stringify(result, null, 2) + '\n');
  } catch (error) { process.stderr.write(error.message + '\n'); process.exitCode = 1; }
}
