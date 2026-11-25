const assert = require("node:assert/strict");
const crypto = require("node:crypto");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const { spawnSync } = require("node:child_process");
const { test } = require("node:test");
const { deduplicateRegularFiles } = require("./package-copy.cjs");
const {
  collectRuntimeNodeModules,
  inspectCliRuntime,
  normalizeDistributionPermissions,
  packageLinuxRelease,
  parseArguments,
  sha256File,
} = require("./release-linux.cjs");

const appRoot = path.resolve(__dirname, "..");
const repoRoot = path.dirname(appRoot);
const pkgVersion = JSON.parse(fs.readFileSync(path.join(appRoot, "package.json"), "utf8")).version;

function writeExecutable(file, body) {
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, body);
  fs.chmodSync(file, 0o755);
}

function createFakeCliDir(root, { platform = "linux", architecture = "x64", version = pkgVersion, duplicatePayload = false } = {}) {
  const cliDir = path.join(root, "cli");
  const runtimeDir = path.join(cliDir, "runtime");
  fs.mkdirSync(path.join(runtimeDir, "bin"), { recursive: true });
  fs.mkdirSync(path.join(runtimeDir, "registry", "interface"), { recursive: true });
  fs.mkdirSync(path.join(runtimeDir, "local-packages", "registry", "interface"), { recursive: true });
  fs.mkdirSync(path.join(runtimeDir, "local-packages", "packages"), { recursive: true });
  fs.mkdirSync(path.join(runtimeDir, "local-packages", "packs"), { recursive: true });
  fs.writeFileSync(path.join(cliDir, "shared.cjs"), "module.exports = {};\n");
  fs.writeFileSync(path.join(runtimeDir, "registry", ".octopus-source.json"), JSON.stringify({ schema_version: 1, id: "baseline" }));
  fs.writeFileSync(path.join(runtimeDir, "local-packages", "registry", ".octopus-source.json"), JSON.stringify({ schema_version: 1, id: "local" }));

  const binA = path.join(runtimeDir, "bin", "octopus");
  const binB = path.join(runtimeDir, "bin", "octopus-linux-process-authority");
  writeExecutable(binA, "#!/bin/sh\necho ok\n");
  writeExecutable(binB, "#!/bin/sh\necho authority\n");
  const binaries = [
    { path: "bin/octopus", bytes: fs.statSync(binA).size, sha256: sha256File(binA) },
    { path: "bin/octopus-linux-process-authority", bytes: fs.statSync(binB).size, sha256: sha256File(binB) },
  ];
  if (duplicatePayload) {
    const payload = Buffer.alloc(16 * 1024, 0x5a);
    const dup1 = path.join(runtimeDir, "local-packages", "registry", "tool", "a", "shared-bin");
    const dup2 = path.join(runtimeDir, "local-packages", "registry", "tool", "b", "shared-bin");
    writeExecutable(dup1, payload);
    writeExecutable(dup2, payload);
    binaries.push(
      { path: "local-packages/registry/tool/a/shared-bin", bytes: payload.length, sha256: sha256File(dup1) },
      { path: "local-packages/registry/tool/b/shared-bin", bytes: payload.length, sha256: sha256File(dup2) },
    );
  }
  fs.writeFileSync(path.join(runtimeDir, "runtime-build-receipt.json"), JSON.stringify({
    schema: "octopus.cli_runtime_distribution",
    schema_version: 1,
    package_version: version,
    platform,
    architecture,
    target: architecture === "x64" ? "x86_64-unknown-linux-gnu" : "aarch64-unknown-linux-gnu",
    binaries,
  }, null, 2));
  return cliDir;
}

test("parseArguments validates paths and collectRuntimeNodeModules resolves closure", () => {
  assert.throws(() => parseArguments(["--cli-dir=/tmp/cli", "--output=/tmp/cli/out"]), /outside the CLI build directory/);
  assert.throws(() => parseArguments(["--unknown"]), /Usage:/);
  const modules = collectRuntimeNodeModules(path.join(appRoot, "node_modules"));
  assert.ok(modules.includes("ajv"));
  assert.ok(modules.includes("ws"));
  assert.ok(modules.includes("@noble/hashes"));
});

test("inspectCliRuntime fails closed on non-linux platform, version mismatch, and tampered binary", () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "oct-rel-linux-inspect-"));
  try {
    const darwinCli = createFakeCliDir(path.join(root, "darwin"), { platform: "darwin" });
    assert.throws(() => inspectCliRuntime(darwinCli, pkgVersion), /linux runtime receipt/);

    const badVerCli = createFakeCliDir(path.join(root, "badver"), { version: "9.9.9" });
    assert.throws(() => inspectCliRuntime(badVerCli, pkgVersion), /does not match/);

    const okCli = createFakeCliDir(path.join(root, "ok"));
    assert.equal(inspectCliRuntime(okCli, pkgVersion).architecture, "x64");
    fs.writeFileSync(path.join(okCli, "runtime", "bin", "octopus"), "#!/bin/sh\necho tampered\n");
    fs.chmodSync(path.join(okCli, "runtime", "bin", "octopus"), 0o755);
    assert.throws(() => inspectCliRuntime(okCli, pkgVersion), /size|sha256/);
  } finally {
    fs.rmSync(root, { recursive: true, force: true });
  }
});

test("deduplicateRegularFiles hardlinks identical >=4KiB files and preserves regular files", () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "oct-dedup-copy-"));
  try {
    const body = crypto.randomBytes(12 * 1024);
    const a = path.join(root, "a/bin");
    const b = path.join(root, "b/bin");
    const c = path.join(root, "c/small");
    const d = path.join(root, "d/small");
    writeExecutable(a, body);
    writeExecutable(b, body);
    fs.mkdirSync(path.dirname(c), { recursive: true });
    fs.mkdirSync(path.dirname(d), { recursive: true });
    fs.writeFileSync(c, "tiny");
    fs.writeFileSync(d, "tiny");
    const stats = deduplicateRegularFiles(root);
    assert.equal(stats.linkedFiles, 1);
    assert.equal(stats.deduplicatedGroups, 1);
    assert.equal(stats.savedBytes, body.length);
    const sa = fs.lstatSync(a), sb = fs.lstatSync(b);
    assert.equal(sa.isFile() && !sa.isSymbolicLink(), true);
    assert.equal(sa.ino, sb.ino);
    assert.notEqual(fs.lstatSync(c).ino, fs.lstatSync(d).ino);
  } finally {
    fs.rmSync(root, { recursive: true, force: true });
  }
});

test("packageLinuxRelease dry-run and full packaging preserve 0755 modes, hardlinks, installer, and checksums", () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "oct-rel-linux-pkg-"));
  try {
    const cliDir = createFakeCliDir(root, { duplicatePayload: true });
    const outDir = path.join(root, "release-out");
    const dry = packageLinuxRelease({ appRoot, repoRoot, cliDir, output: outDir, dryRun: true });
    assert.equal(dry.applied, false);
    assert.equal(fs.existsSync(outDir), false);

    // Restrict runtime dir to 0700 to verify normalizeDistributionPermissions promotes to 0755.
    fs.chmodSync(path.join(cliDir, "runtime"), 0o700);
    const res = packageLinuxRelease({ appRoot, repoRoot, cliDir, output: outDir });
    assert.equal(res.applied, true);
    assert.ok(res.deduplication.deduplicatedFiles >= 1);
    assert.equal(fs.existsSync(res.tarball), true);
    assert.equal(fs.existsSync(res.sha256File), true);
    assert.equal(fs.existsSync(res.installer), true);
    assert.equal(fs.existsSync(res.checksums), true);
    const checksumsText = fs.readFileSync(res.checksums, "utf8");
    assert.ok(checksumsText.includes(path.basename(res.tarball)));
    assert.ok(checksumsText.includes("install-linux.sh"));

    const unpack = path.join(root, "unpack");
    fs.mkdirSync(unpack, { recursive: true });
    const untar = spawnSync("tar", ["-xzf", res.tarball, "-C", unpack], { encoding: "utf8" });
    assert.equal(untar.status, 0, untar.stderr);
    const extracted = path.join(unpack, res.stem);
    assert.equal(fs.lstatSync(extracted).mode & 0o777, 0o755);
    assert.equal(fs.lstatSync(path.join(extracted, "app/build/cli/runtime")).mode & 0o777, 0o755);
    const dupA = fs.lstatSync(path.join(extracted, "app/build/cli/runtime/local-packages/registry/tool/a/shared-bin"));
    const dupB = fs.lstatSync(path.join(extracted, "app/build/cli/runtime/local-packages/registry/tool/b/shared-bin"));
    assert.equal(dupA.ino, dupB.ino, "tarball extraction must preserve hardlink deduplication");
    normalizeDistributionPermissions(extracted);
  } finally {
    fs.rmSync(root, { recursive: true, force: true });
  }
});
