const assert = require("node:assert/strict");
const crypto = require("node:crypto");
const fs = require("node:fs");
const http = require("node:http");
const os = require("node:os");
const path = require("node:path");
const { spawn, spawnSync } = require("node:child_process");
const { test } = require("node:test");

const releaseDir = path.resolve(__dirname, "..", "release");
const installSh = path.join(releaseDir, "install.sh");
const installLinuxSh = path.join(releaseDir, "install-linux.sh");

function writeExecutable(file, body) {
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, body);
  fs.chmodSync(file, 0o755);
}

function sha256Buffer(buffer) {
  return crypto.createHash("sha256").update(buffer).digest("hex");
}

function createFakeHostBin(root, { osName = "Linux", arch = "x86_64", kernel = "7.0.0-44-generic", nodeVersion = null, systemctlActive = false } = {}) {
  const binDir = path.join(root, "fake-bin");
  fs.mkdirSync(binDir, { recursive: true });
  writeExecutable(path.join(binDir, "uname"), `#!/bin/sh
case "$1" in
  -s) printf '%s\\n' '${osName}' ;;
  -m) printf '%s\\n' '${arch}' ;;
  -r) printf '%s\\n' '${kernel}' ;;
  *) printf '%s\\n' '${osName}' ;;
esac
`);
  if (nodeVersion !== null) {
    writeExecutable(path.join(binDir, "node"), `#!/bin/sh
if [ "$1" = "--version" ]; then printf '%s\\n' '${nodeVersion}'; exit 0; fi
exec "${process.execPath}" "$@"
`);
  }
  if (systemctlActive) {
    writeExecutable(path.join(binDir, "systemctl"), `#!/bin/sh
if [ "$1" = "is-active" ] && [ "$2" = "--quiet" ] && [ "$3" = "octopus-process-authority.service" ]; then exit 0; fi
exit 1
`);
  }
  return binDir;
}

function buildSyntheticTarball(root, { version = "0.1.0", arch = "x64", marker = "v1" } = {}) {
  const stem = `Octopus-${version}-linux-${arch}`;
  const stageParent = path.join(root, `stage-${marker}`);
  const payload = path.join(stageParent, stem);
  fs.mkdirSync(path.join(payload, "app", "cli"), { recursive: true });
  fs.mkdirSync(path.join(payload, "app", "build", "cli", "runtime", "bin"), { recursive: true });
  fs.mkdirSync(path.join(payload, "linux-process-authority"), { recursive: true });
  writeExecutable(path.join(payload, "octopus"), `#!/bin/sh
if [ "$1" = "--version" ]; then printf 'Octopus ${version} (${marker})\\n'; exit 0; fi
printf 'ok:${marker}\\n'
`);
  fs.writeFileSync(path.join(payload, "app", "cli", "index.cjs"), "module.exports = {};\n");
  fs.writeFileSync(path.join(payload, "app", "build", "cli", "runtime", "runtime-build-receipt.json"), JSON.stringify({
    schema: "octopus.cli_runtime_distribution",
    schema_version: 1,
    package_version: version,
    platform: "linux",
    architecture: arch,
  }));
  writeExecutable(path.join(payload, "app", "build", "cli", "runtime", "bin", "octopus"), "#!/bin/sh\nexit 0\n");
  const shared = Buffer.alloc(8192, marker.charCodeAt(0));
  fs.writeFileSync(path.join(payload, "a.bin"), shared);
  fs.linkSync(path.join(payload, "a.bin"), path.join(payload, "b.bin"));
  const tarball = path.join(root, `${stem}-${marker}.tar.gz`);
  const tarRes = spawnSync("tar", ["-czf", tarball, "-C", stageParent, stem], { encoding: "utf8" });
  assert.equal(tarRes.status, 0, tarRes.stderr);
  const bytes = fs.readFileSync(tarball);
  return { stem, asset: `${stem}.tar.gz`, bytes, sha256: sha256Buffer(bytes) };
}

async function withHttpServer(filesByName, fn) {
  const server = http.createServer((req, res) => {
    const name = decodeURIComponent((req.url || "/").replace(/^\/+/, ""));
    if (!Object.prototype.hasOwnProperty.call(filesByName, name)) {
      res.writeHead(404);
      res.end("not found");
      return;
    }
    res.writeHead(200, { "content-type": "application/octet-stream" });
    res.end(filesByName[name]);
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const { port } = server.address();
  try {
    return await fn(`http://127.0.0.1:${port}`);
  } finally {
    await new Promise((resolve) => server.close(resolve));
  }
}

function runShell(scriptPath, args, { env = {} } = {}) {
  return spawnSync("sh", [scriptPath, ...args], {
    encoding: "utf8",
    env: { ...process.env, ...env },
  });
}

function runShellAsync(scriptPath, args, { env = {} } = {}) {
  return new Promise((resolve, reject) => {
    const child = spawn("sh", [scriptPath, ...args], {
      env: { ...process.env, ...env },
      stdio: ["ignore", "pipe", "pipe"],
    });
    let stdout = "", stderr = "";
    child.stdout.on("data", (d) => { stdout += d; });
    child.stderr.on("data", (d) => { stderr += d; });
    child.on("error", reject);
    child.on("close", (status) => resolve({ status, stdout, stderr }));
  });
}

test("install.sh and install-linux.sh are executable, pass sh -n, and default to miuuyy/Octopus-SI-OS", () => {
  for (const script of [installSh, installLinuxSh]) {
    const stat = fs.lstatSync(script);
    assert.equal(stat.isFile(), true);
    assert.ok((stat.mode & 0o111) !== 0);
    const syntax = spawnSync("sh", ["-n", script], { encoding: "utf8" });
    assert.equal(syntax.status, 0, syntax.stderr);
    const body = fs.readFileSync(script, "utf8");
    assert.match(body, /miuuyy\/Octopus-SI-OS/);
  }
  const help = runShell(installLinuxSh, ["--help"]);
  assert.equal(help.status, 0, help.stderr);
  assert.match(help.stdout, /--check/);
  assert.equal(runShell(installLinuxSh, ["--nope"]).status, 2);
});

test("install-linux.sh --check --json is read-only and reports ready, blockers, and non-member authority warning", () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "oct-inst-check-"));
  try {
    const prefix = path.join(root, "opt", "octopus");
    const binDir = path.join(root, "bin");
    const readyBin = createFakeHostBin(root, { nodeVersion: "v24.11.0" });
    const env = { PATH: `${readyBin}:${process.env.PATH}`, OCTOPUS_NODE: path.join(readyBin, "node"), OCTOPUS_MIN_FREE_MB: "1" };

    const okRes = runShell(installLinuxSh, ["--check", "--json", "--prefix", prefix, "--bin-dir", binDir], { env });
    assert.equal(okRes.status, 0, `${okRes.stdout}\n${okRes.stderr}`);
    const okJson = JSON.parse(okRes.stdout);
    assert.equal(okJson.ok, true);
    assert.equal(okJson.hard_failures, 0);
    assert.equal(fs.existsSync(prefix), false, "--check must not create prefix");
    assert.equal(fs.existsSync(binDir), false, "--check must not create binDir");

    // Blocker: Node 22 + unsupported arch.
    const badBin = createFakeHostBin(path.join(root, "bad"), { arch: "aarch64", nodeVersion: "v22.14.0" });
    const badRes = runShell(installLinuxSh, ["--check", "--json", "--prefix", prefix, "--bin-dir", binDir], {
      env: { PATH: `${badBin}:${process.env.PATH}`, OCTOPUS_NODE: path.join(badBin, "node"), OCTOPUS_MIN_FREE_MB: "1" },
    });
    assert.equal(badRes.status, 1);
    const badJson = JSON.parse(badRes.stdout);
    assert.equal(badJson.ok, false);
    const byId = Object.fromEntries(badJson.checks.map((c) => [c.id, c]));
    assert.equal(byId.arch.status, "fail");
    assert.equal(byId.node.status, "fail");

    // Active authority service without octopus-process group membership warns clearly.
    const authBin = createFakeHostBin(path.join(root, "auth"), { nodeVersion: "v24.11.0", systemctlActive: true });
    const authRes = runShell(installLinuxSh, ["--check", "--json", "--prefix", prefix, "--bin-dir", binDir], {
      env: { PATH: `${authBin}:${process.env.PATH}`, OCTOPUS_NODE: path.join(authBin, "node"), OCTOPUS_MIN_FREE_MB: "1" },
    });
    assert.equal(authRes.status, 0, authRes.stderr);
    const authCheck = JSON.parse(authRes.stdout).checks.find((c) => c.id === "process_authority");
    assert.equal(authCheck.status, "warn");
    assert.match(authCheck.detail, /octopus-process/);
  } finally {
    fs.rmSync(root, { recursive: true, force: true });
  }
});

test("install-linux.sh verifies SHA-256, installs & atomically upgrades, preserves hardlinks, and refuses tampering or foreign binaries", async () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "oct-inst-e2e-"));
  try {
    const fakeBin = createFakeHostBin(root, { nodeVersion: "v24.11.0" });
    const prefix = path.join(root, "install");
    const binDir = path.join(root, "bin");
    const v1 = buildSyntheticTarball(root, { version: "0.1.0", marker: "v1" });
    const v2 = buildSyntheticTarball(root, { version: "0.1.0", marker: "v2" });
    const files = {
      [v1.asset]: v1.bytes,
      "checksums.txt": `${v1.sha256}  ${v1.asset}\n`,
    };
    await withHttpServer(files, async (baseUrl) => {
      const env = {
        PATH: `${fakeBin}:${process.env.PATH}`,
        OCTOPUS_NODE: path.join(fakeBin, "node"),
        OCTOPUS_RELEASE_BASE_URL: baseUrl,
        OCTOPUS_MIN_FREE_MB: "1",
      };
      const first = await runShellAsync(installLinuxSh, ["--json", "--version", "0.1.0", "--prefix", prefix, "--bin-dir", binDir], { env });
      assert.equal(first.status, 0, `${first.stdout}\n${first.stderr}`);
      const parsed = JSON.parse(first.stdout);
      assert.equal(parsed.ok, true);
      assert.equal(parsed.sha256, v1.sha256);
      assert.equal(fs.readlinkSync(path.join(binDir, "octopus")), path.join(prefix, "octopus"));
      assert.equal(fs.lstatSync(path.join(prefix, "a.bin")).ino, fs.lstatSync(path.join(prefix, "b.bin")).ino);

      // Tampered checksum must fail closed without touching existing v1 install.
      files["checksums.txt"] = `${"0".repeat(64)}  ${v1.asset}\n`;
      const badSha = await runShellAsync(installLinuxSh, ["--version", "0.1.0", "--prefix", prefix, "--bin-dir", binDir], { env });
      assert.notEqual(badSha.status, 0);
      assert.match(badSha.stderr, /SHA-256 verification failed/);
      assert.match(spawnSync(path.join(prefix, "octopus"), ["--version"], { encoding: "utf8" }).stdout, /\(v1\)/);

      // Valid atomic upgrade to v2 replaces prefix cleanly.
      files[v2.asset] = v2.bytes;
      files["checksums.txt"] = `${v2.sha256}  ${v2.asset}\n`;
      const upgraded = await runShellAsync(installLinuxSh, ["--json", "--version", "0.1.0", "--prefix", prefix, "--bin-dir", binDir], { env });
      assert.equal(upgraded.status, 0, upgraded.stderr);
      assert.match(spawnSync(path.join(prefix, "octopus"), ["--version"], { encoding: "utf8" }).stdout, /\(v2\)/);

      // Non-Octopus binary at --bin-dir/octopus must not be overwritten.
      const foreignBinDir = path.join(root, "foreign-bin");
      writeExecutable(path.join(foreignBinDir, "octopus"), "#!/bin/sh\necho foreign\n");
      const conflict = await runShellAsync(installLinuxSh, ["--version", "0.1.0", "--prefix", prefix, "--bin-dir", foreignBinDir], { env });
      assert.notEqual(conflict.status, 0);
      assert.match(fs.readFileSync(path.join(foreignBinDir, "octopus"), "utf8"), /foreign/);
    });
  } finally {
    fs.rmSync(root, { recursive: true, force: true });
  }
});

test("install.sh dispatches to install-linux.sh on Linux locally and via checksum-verified remote download", async () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "oct-inst-dispatch-"));
  try {
    const fakeBin = createFakeHostBin(root, { nodeVersion: "v24.11.0" });
    const prefix = path.join(root, "opt");
    const binDir = path.join(root, "bin");
    const env = { PATH: `${fakeBin}:${process.env.PATH}`, OCTOPUS_NODE: path.join(fakeBin, "node"), OCTOPUS_MIN_FREE_MB: "1" };

    // Local sibling dispatch.
    const localRes = runShell(installSh, ["--check", "--json", "--prefix", prefix, "--bin-dir", binDir], { env });
    assert.equal(localRes.status, 0, localRes.stderr);
    assert.equal(JSON.parse(localRes.stdout).mode, "check");

    // Remote checksum-verified dispatch (isolated copy without sibling install-linux.sh).
    const loneInstallSh = path.join(root, "isolated-install.sh");
    fs.copyFileSync(installSh, loneInstallSh);
    const linuxScriptBytes = fs.readFileSync(installLinuxSh);
    const files = {
      "install-linux.sh": linuxScriptBytes,
      "checksums.txt": `${sha256Buffer(linuxScriptBytes)}  install-linux.sh\n`,
    };
    await withHttpServer(files, async (baseUrl) => {
      const remoteEnv = { ...env, OCTOPUS_RELEASE_BASE_URL: baseUrl };
      const ok = await runShellAsync(loneInstallSh, ["--check", "--json", "--version", "0.1.0", "--prefix", prefix, "--bin-dir", binDir], { env: remoteEnv });
      assert.equal(ok.status, 0, ok.stderr);
      assert.equal(JSON.parse(ok.stdout).ok, true);

      files["checksums.txt"] = `${"f".repeat(64)}  install-linux.sh\n`;
      const bad = await runShellAsync(loneInstallSh, ["--check", "--version", "0.1.0", "--prefix", prefix, "--bin-dir", binDir], { env: remoteEnv });
      assert.notEqual(bad.status, 0);
      assert.match(bad.stderr, /SHA-256 verification failed/);
    });
  } finally {
    fs.rmSync(root, { recursive: true, force: true });
  }
});
