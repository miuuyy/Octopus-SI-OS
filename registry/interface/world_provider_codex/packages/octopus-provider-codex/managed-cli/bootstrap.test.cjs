const test = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const os = require("node:os");
const crypto = require("node:crypto");
const { spawnSync } = require("node:child_process");
const cli = require("./bootstrap.cjs");

function fixture(root, version) {
  const source = path.join(root, version); fs.mkdirSync(source);
  const entry = "codex-aarch64-apple-darwin", binary = path.join(source, entry);
  fs.writeFileSync(binary, `#!/bin/sh\nif [ "$1" = "--version" ]; then echo 'codex-cli ${version}'; exit 0; fi\nif [ "$1" = "app-server" ] && [ "$2" = "generate-json-schema" ]; then\nmkdir -p "$5"\nprintf '{}\\n' > "$5/SourceProbe.json"\nprintf '{"oneOf":[{"properties":{"method":{"enum":["turn/completed"]}}}]}\\n' > "$5/ServerNotification.json"\nexit 0\nfi\nexit 1\n`, { mode: 0o755 });
  const archive = path.join(source, "official.tar.gz");
  assert.equal(spawnSync("tar", ["-czf", archive, "-C", source, entry]).status, 0);
  const manifest = {
    schema_version: 2, version, tag: `rust-v${version}`, release_url: `https://github.com/openai/codex/releases/tag/rust-v${version}`,
    platforms: { "darwin-arm64": { archive_entry: entry, archive_bytes: fs.statSync(archive).size,
      archive_sha256: cli.digest(archive), url: `https://github.com/openai/codex/releases/download/rust-v${version}/${entry}.tar.gz` } },
    schemas: { "SourceProbe.json": crypto.createHash("sha256").update("{}\n").digest("hex") },
    legal: {}, notifications: { mirrored: ["turn/completed"], opt_out: ["thread/queue/changed"] },
  };
  fs.mkdirSync(path.join(source, "upstream"));
  for (const name of ["LICENSE", "NOTICE"]) {
    const file = path.join(source, "upstream", name);
    fs.writeFileSync(file, `${name} fixture for ${version}\n`);
    manifest.legal[name] = { source_url: `https://raw.githubusercontent.com/openai/codex/rust-v${version}/${name}`, sha256: cli.digest(file) };
  }
  const helperEntry = entry.replace("codex-", "codex-code-mode-host-");
  fs.writeFileSync(path.join(source, helperEntry), `#!/bin/sh\necho 'code mode ${version}'\n`, {mode: 0o755});
  const helperArchive = path.join(source, "host.tar.gz");
  assert.equal(spawnSync("tar", ["-czf", helperArchive, "-C", source, helperEntry]).status, 0);
  manifest.platforms["darwin-arm64"].companions = { "codex-code-mode-host": {
    archive_entry: helperEntry, archive_bytes: fs.statSync(helperArchive).size, archive_sha256: cli.digest(helperArchive),
    url: `https://github.com/openai/codex/releases/download/rust-v${version}/${helperEntry}.tar.gz`,
  } };
  const manifestPath = path.join(source, "release.json"); fs.writeFileSync(manifestPath, JSON.stringify(manifest));
  return { manifestPath, archive, manifest, fetchArchive: async (url, target) => fs.copyFileSync(url.includes("code-mode-host") ? helperArchive : archive, target) };
}

test("fresh acquisition, stable update, downgrade refusal and integrity admission", async () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "octopus-codex-installer-test-"));
  try {
    const destination = path.join(root, "managed-bin"), old = fixture(root, "0.166.0"), next = fixture(root, "0.167.0");
    const first = await cli.materialize({ destination, platform: "darwin-arm64", ...old });
    assert.equal(first.version, "0.166.0");
    assert.deepEqual(cli.verifyInstallation(destination), first);
    const unchanged = fs.readFileSync(path.join(destination, "codex"));
    const reused = await cli.materialize({ destination, platform: "darwin-arm64", manifestPath: old.manifestPath,
      fetchArchive: async () => { throw new Error("no download for verified installed release"); } });
    assert.deepEqual(reused, first);
    const updated = await cli.materialize({ destination, platform: "darwin-arm64", ...next });
    assert.equal(updated.version, "0.167.0"); assert.notDeepEqual(fs.readFileSync(path.join(destination, "codex")), unchanged);
    await assert.rejects(cli.materialize({ destination, platform: "darwin-arm64", ...old }), /downgrade/);
    assert.deepEqual(cli.verifyInstallation(destination), updated);
    const imported = path.join(root, "registry", "interface", "world_provider_codex", "packages", "bin");
    cli.copyInstallation(destination, imported); assert.deepEqual(cli.verifyInstallation(imported), updated);
    for (const name of ["LICENSE", "NOTICE"]) assert.deepEqual(fs.readFileSync(path.join(imported, name)), fs.readFileSync(path.join(destination, name)));
    const helper = path.join(imported, "codex-code-mode-host");
    fs.unlinkSync(helper);
    assert.throws(() => cli.verifyInstallation(imported), /ENOENT/);
    cli.copyInstallation(destination, imported);
    fs.appendFileSync(helper, "corrupt");
    assert.throws(() => cli.verifyInstallation(imported), /differs from its receipt/);
    cli.copyInstallation(destination, imported);
    fs.appendFileSync(path.join(imported, "NOTICE"), "corrupt");
    assert.throws(() => cli.verifyInstallation(imported), /attribution differs/);
    fs.appendFileSync(path.join(destination, "codex"), "corrupt");
    await assert.rejects(cli.materialize({ destination, platform: "darwin-arm64", ...next }), /differs from its receipt/);
    assert.equal(fs.existsSync(path.join(destination, ".codex-cli-install.lock")), false);
  } finally { fs.rmSync(root, { recursive: true, force: true }); }
});

test("checksum and protocol failures never publish an executable", async () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "octopus-codex-installer-test-"));
  try {
    const source = fixture(root, "0.166.0"), destination = path.join(root, "bin");
    await assert.rejects(cli.materialize({ destination, platform: "darwin-arm64", manifestPath: source.manifestPath,
      fetchArchive: async (_, target) => { fs.copyFileSync(source.archive, target); fs.appendFileSync(target, "wrong"); } }), /SHA-256/);
    assert.equal(fs.existsSync(path.join(destination, "codex")), false);
    source.manifest.notifications.mirrored = ["turn/started"];
    fs.writeFileSync(source.manifestPath, JSON.stringify(source.manifest));
    await assert.rejects(cli.materialize({ destination, platform: "darwin-arm64", ...source }), /neither mirrors nor opts out of: turn\/completed$/);
    assert.equal(fs.existsSync(path.join(destination, "codex")), false);
    source.manifest.notifications.mirrored = ["turn/completed"];
    source.manifest.schemas["SourceProbe.json"] = "f".repeat(64);
    fs.writeFileSync(source.manifestPath, JSON.stringify(source.manifest));
    await assert.rejects(cli.materialize({ destination, platform: "darwin-arm64", ...source }), /validated schema/);
    assert.equal(fs.existsSync(path.join(destination, "codex")), false);
    assert.deepEqual(fs.readdirSync(destination), []);
  } finally { fs.rmSync(root, { recursive: true, force: true }); }
});

test("interrupted receipt publication resumes from exact installed bytes without reacquisition", async () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "octopus-codex-installer-test-"));
  try {
    const source = fixture(root, "0.166.0"), destination = path.join(root, "bin");
    const receipt = await cli.materialize({ destination, platform: "darwin-arm64", ...source });
    fs.unlinkSync(path.join(destination, cli.RECEIPT));
    fs.writeFileSync(path.join(destination, ".codex-cli-pending.json"), JSON.stringify({ candidate: ".codex-stage-exact/codex", receipt }));
    const exited = spawnSync(process.execPath, ["-e", "process.exit(0)"]);
    assert.equal(exited.status, 0);
    fs.writeFileSync(path.join(destination, ".codex-cli-install.lock"), JSON.stringify({ schema_version: 1, pid: exited.pid, version: "0.166.0" }));
    const recovered = await cli.materialize({ destination, platform: "darwin-arm64", manifestPath: source.manifestPath,
      fetchArchive: async () => { throw new Error("no duplicate external acquisition"); } });
    assert.deepEqual(recovered, receipt); assert.deepEqual(cli.verifyInstallation(destination), receipt);
    fs.writeFileSync(path.join(destination, ".codex-cli-install.lock"), JSON.stringify({ schema_version: 1, pid: process.pid, version: "0.166.0" }));
    await assert.rejects(cli.materialize({ destination, platform: "darwin-arm64", ...source }), /already in progress/);
    fs.unlinkSync(path.join(destination, ".codex-cli-install.lock"));
    fs.writeFileSync(path.join(destination, ".codex-cli-pending.json"), JSON.stringify({ candidate: "../outside/codex", receipt }));
    await assert.rejects(cli.materialize({ destination, platform: "darwin-arm64", ...source }), /invalid or different release evidence/);
    assert.equal(fs.existsSync(path.join(destination, ".codex-cli-pending.json")), true);
  } finally { fs.rmSync(root, { recursive: true, force: true }); }
});
