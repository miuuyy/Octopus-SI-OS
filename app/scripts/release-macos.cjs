const fs = require("node:fs");
const path = require("node:path");
const crypto = require("node:crypto");
const { spawnSync } = require("node:child_process");
const { cloneFiles } = require("./copy-macos.cjs");

const appRoot = path.resolve(__dirname, "..");
const repoRoot = path.dirname(appRoot);
const releaseSources = path.join(appRoot, "release");
const managedCodex = require(path.join(repoRoot, "registry", "interface", "world_provider_codex", "packages", "octopus-provider-codex", "managed-cli", "bootstrap.cjs"));
const bundleIdentifier = "com.octopus.desktop";
const minimumSystemVersion = "14.0";
const architectures = { arm64: "arm64", x86_64: "x64" };
const blockBytes = 4096;

function main(argv) {
  const options = parseArguments(argv);
  const version = JSON.parse(fs.readFileSync(path.join(appRoot, "package.json"), "utf8")).version;
  if (!/^\d+\.\d+\.\d+$/.test(version)) throw new Error(`package version must be a three-part numeric version: ${version}`);
  const architecture = inspectBundle(options.bundle, version);
  const bundleTree = treeManifest(options.bundle);
  fs.rmSync(options.output, { recursive: true, force: true });
  fs.mkdirSync(options.output, { recursive: true });
  const image = writeDiskImage(options, `Octopus-${version}-macos-${architecture}.dmg`, version, bundleTree);
  const installer = path.join(options.output, "install.sh");
  fs.copyFileSync(path.join(releaseSources, "install.sh"), installer);
  fs.chmodSync(installer, 0o755);
  const assets = [image, installer];
  writeChecksums(options.output, assets);
  const notes = renderNotes(options.output, version, architecture);
  for (const asset of assets) console.log(`${path.basename(asset)}\t${fs.statSync(asset).size}`);
  console.log(`Release assets for ${version} (${architecture}) are in ${options.output}; notes: ${notes}`);
}

function parseArguments(argv) {
  const options = {
    bundle: path.join(appRoot, "build", "Octopus.app"),
    output: path.join(appRoot, "build", "release"),
  };
  for (const argument of argv) {
    const [key, value] = argument.split(/=(.*)/s, 2);
    if (key === "--bundle" && value) options.bundle = path.resolve(value);
    else if (key === "--output" && value) options.output = path.resolve(value);
    else throw new Error("Usage: release-macos.cjs [--bundle=<Octopus.app>] [--output=<directory>]");
  }
  if (path.basename(options.bundle) !== "Octopus.app") throw new Error(`release bundle must be named Octopus.app: ${options.bundle}`);
  if (options.output === options.bundle || options.output.startsWith(`${options.bundle}${path.sep}`)) throw new Error("release output must be outside the bundle");
  return options;
}

function inspectBundle(bundle, version) {
  const contents = path.join(bundle, "Contents");
  const plist = path.join(contents, "Info.plist");
  const expected = {
    CFBundleExecutable: "Octopus",
    CFBundleIdentifier: bundleIdentifier,
    CFBundleShortVersionString: version,
    CFBundleVersion: version,
    LSMinimumSystemVersion: minimumSystemVersion,
  };
  for (const [key, value] of Object.entries(expected)) {
    const actual = capture("plutil", ["-extract", key, "raw", "-o", "-", plist]);
    if (actual !== value) throw new Error(`release bundle ${key} is '${actual}', expected '${value}'`);
  }
  const archs = capture("lipo", ["-archs", path.join(contents, "MacOS", "Octopus")]).split(/\s+/);
  if (archs.length !== 1 || !architectures[archs[0]]) throw new Error(`release bundle must be a single supported architecture: ${archs.join(" ")}`);
  const architecture = architectures[archs[0]];
  const engine = JSON.parse(fs.readFileSync(path.join(contents, "Resources", "browser-engine.json"), "utf8"));
  if (engine.architecture !== architecture) throw new Error(`browser engine architecture ${engine.architecture} differs from the bundle (${architecture})`);
  const installationBin = path.join(contents, "Resources", "app", "bin");
  const runtime = fs.lstatSync(path.join(installationBin, "octopus"));
  if (!runtime.isFile() || !(runtime.mode & 0o111)) throw new Error("release bundle has no runnable runtime executable");
  managedCodex.verifyInstallation(installationBin);
  run("codesign", ["--verify", "--deep", "--strict", bundle]);
  return architecture;
}

function writeDiskImage(options, name, version, bundleTree) {
  const image = path.join(options.output, name);
  const stage = fs.mkdtempSync(path.join(options.output, ".image-stage-"));
  try {
    const sparse = path.join(stage, "Octopus.sparseimage");
    run("hdiutil", ["create", "-quiet", "-type", "SPARSE", "-fs", "APFS", "-volname", `Octopus ${version}`,
      "-size", `${imageMegabytes(bundleTree)}m`, sparse]);
    withMountedImage(sparse, path.join(stage, "mount"), false, (volume) => {
      const applyModes = materializeDeduplicated(options.bundle, bundleTree, volume);
      run("xattr", ["-cr", path.join(volume, "Octopus.app")]);
      applyModes();
      fs.symlinkSync("/Applications", path.join(volume, "Applications"));
    });
    run("hdiutil", ["convert", "-quiet", sparse, "-format", "ULMO", "-o", image]);
  } finally {
    fs.rmSync(stage, { recursive: true, force: true });
  }
  run("hdiutil", ["verify", "-quiet", image]);
  withMountedImage(image, `${image}.verify`, true, (volume) => {
    const applications = path.join(volume, "Applications");
    if (!fs.lstatSync(applications).isSymbolicLink() || fs.readlinkSync(applications) !== "/Applications") {
      throw new Error("disk image has no Applications link");
    }
    compareTrees(bundleTree, treeManifest(path.join(volume, "Octopus.app")), path.basename(image));
    run("codesign", ["--verify", "--deep", "--strict", path.join(volume, "Octopus.app")]);
  });
  return image;
}

function imageMegabytes(bundleTree) {
  const unique = new Map();
  let entries = 0;
  for (const value of bundleTree.values()) {
    const entry = JSON.parse(value);
    entries += 1;
    if (entry.type === "file") unique.set(entry.sha256, Math.ceil(entry.size / blockBytes) * blockBytes);
  }
  const bytes = [...unique.values()].reduce((sum, size) => sum + size, 0) + entries * blockBytes;
  return Math.ceil((bytes * 1.1) / (1024 * 1024)) + 256;
}

function materializeDeduplicated(bundle, bundleTree, volume) {
  const sourceParent = path.dirname(bundle);
  const written = new Map();
  const directories = [];
  const files = [];
  const duplicates = [];
  for (const [relative, value] of bundleTree) {
    const entry = JSON.parse(value);
    const destination = path.join(volume, ...relative.split("/"));
    if (entry.type === "directory") {
      fs.mkdirSync(destination, { mode: 0o755 });
      directories.push([destination, entry.mode]);
    } else if (entry.type === "symlink") {
      fs.symlinkSync(entry.target, destination);
    } else {
      const first = written.get(entry.sha256);
      if (first) {
        duplicates.push({ source: first, destination, mode: entry.mode });
        continue;
      }
      fs.copyFileSync(path.join(sourceParent, ...relative.split("/")), destination, fs.constants.COPYFILE_EXCL);
      fs.chmodSync(destination, entry.mode | 0o200);
      files.push({ destination, mode: entry.mode });
      written.set(entry.sha256, destination);
    }
  }
  cloneFiles(duplicates);
  for (const { destination, mode } of duplicates) fs.chmodSync(destination, mode | 0o200);
  return () => {
    for (const { destination, mode } of [...files, ...duplicates]) fs.chmodSync(destination, mode);
    for (const [directory, mode] of directories.reverse()) fs.chmodSync(directory, mode);
  };
}

function withMountedImage(image, mountRoot, readonly, use) {
  fs.mkdirSync(mountRoot);
  let volume = null;
  try {
    const attach = ["attach", "-nobrowse", "-noautoopen", "-noverify", "-mountrandom", mountRoot, "-plist", image];
    if (readonly) attach.splice(1, 0, "-readonly");
    const plist = capture("hdiutil", attach);
    const attached = JSON.parse(capture("plutil", ["-convert", "json", "-o", "-", "-"], 1024 * 1024, plist));
    volume = attached["system-entities"].map((entity) => entity["mount-point"]).find(Boolean) ?? null;
    if (!volume) throw new Error(`disk image did not mount: ${image}`);
    use(volume);
  } finally {
    if (volume) run("hdiutil", ["detach", "-quiet", volume]);
    fs.rmSync(mountRoot, { recursive: true, force: true });
  }
}

function treeManifest(root) {
  const manifest = new Map();
  const parent = path.dirname(root);
  const visit = (file) => {
    const stat = fs.lstatSync(file);
    const relative = path.relative(parent, file).split(path.sep).join("/");
    let entry;
    if (stat.isSymbolicLink()) entry = { type: "symlink", target: fs.readlinkSync(file) };
    else if (stat.isDirectory()) entry = { type: "directory", mode: stat.mode & 0o7777 };
    else if (stat.isFile()) entry = { type: "file", mode: stat.mode & 0o7777, size: stat.size, sha256: sha256File(file) };
    else throw new Error(`release bundle contains an unsupported entry: ${file}`);
    manifest.set(relative, JSON.stringify(entry));
    if (stat.isDirectory()) for (const name of fs.readdirSync(file).sort()) visit(path.join(file, name));
  };
  visit(root);
  return manifest;
}

function compareTrees(expected, actual, label) {
  const differences = [];
  for (const [file, entry] of expected) if (actual.get(file) !== entry) differences.push(file);
  for (const file of actual.keys()) if (!expected.has(file)) differences.push(file);
  if (differences.length) throw new Error(`${label} differs from the bundle at ${differences.length} path(s), first: ${differences.slice(0, 5).join(", ")}`);
}

function writeChecksums(output, assets) {
  const lines = assets.map((asset) => `${sha256File(asset)}  ${path.basename(asset)}`)
    .sort((left, right) => left.slice(66).localeCompare(right.slice(66)));
  fs.writeFileSync(path.join(output, "checksums.txt"), `${lines.join("\n")}\n`);
}

function renderNotes(output, version, architecture) {
  const template = fs.readFileSync(path.join(releaseSources, "release-notes.md"), "utf8");
  const notes = template.replaceAll("{{version}}", version).replaceAll("{{architecture}}", architecture);
  if (/\{\{[^}]*\}\}/.test(notes)) throw new Error("release notes template has an unknown placeholder");
  const file = path.join(path.dirname(output), `${path.basename(output)}-notes.md`);
  fs.writeFileSync(file, notes);
  return file;
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

function run(command, args) {
  const result = spawnSync(command, args, { stdio: "inherit" });
  if (result.error) throw result.error;
  if (result.status !== 0) throw new Error(`${command} ${args.join(" ")} failed with status ${result.status ?? result.signal}`);
}

function capture(command, args, maxBuffer = 1024 * 1024, input = undefined) {
  const result = spawnSync(command, args, { encoding: "utf8", maxBuffer, input });
  if (result.error) throw result.error;
  if (result.status !== 0) throw new Error(`${command} failed: ${result.stderr.trim() || result.status}`);
  return result.stdout.trim();
}

if (require.main === module) {
  try {
    main(process.argv.slice(2));
  } catch (error) {
    console.error(`macOS release failed: ${error.message}`);
    process.exitCode = 1;
  }
}
