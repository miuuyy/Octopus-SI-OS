const { nativePublication, transactionEntries: retainedTransactionEntries } = require("./support/native-publication.cjs");
const assert = require("node:assert/strict");
const crypto = require("node:crypto");
const fs = require("node:fs/promises");
const fsSync = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const vm = require("node:vm");
const nativeFiles = require("../electron/nativeFiles.cjs");
const { spawnSync, execFileSync } = require("node:child_process");
const { registerLocalPackageMutationGuard, installLocalPackages, repairLocalPackages, removeLocalPackages, readLocalPackageCatalog, recoverLocalPackageTransactions } = require("../electron/localPackages.cjs");
const { assertDeclaredGeneratedOutput, isGeneratedSourcePath, readGeneratedSourcePaths } = require("./generated-source-boundary.cjs");
const { validatePublicPayload } = require("./publication-lib.cjs");

function rootsFor(root) {
  return { publication: nativePublication(), sourceRegistryRoot: path.join(root, "source"), activeRegistryRoot: path.join(root, "active"),
    libraryRoot: path.join(root, "library"), stateRoot: path.join(root, "state") };
}
function ownerFile(roots, active, file) {
  return path.join(active ? roots.activeRegistryRoot : roots.sourceRegistryRoot, "interface", "world_fixture", file);
}
async function json(file, value) {
  await fs.mkdir(path.dirname(file), { recursive: true });
  await fs.writeFile(file, `${JSON.stringify(value, null, 2)}\n`);
}
async function fixture(root) {
  const roots = rootsFor(root);
  await json(ownerFile(roots, false, "interface.json"), { id: "world_fixture", config: { model: "default", effort: "medium" }, contract: 1 });
  await json(ownerFile(roots, false, "application.json"), { schema_version: 3, id: "world_fixture", name: "Fixture", description: "Declared setup fields", developer: "Octopus", version: "1.0.0", category: "provider", interfaces: ["world_fixture"],
    setup: { fields: [{ interface: "world_fixture", kind: "url", label: "Endpoint", pointer: "/config/endpoint", required: false }] } });
  await fs.writeFile(ownerFile(roots, false, "life-bin"), "version one\n", { mode: 0o755 });
  await json(path.join(roots.sourceRegistryRoot, "tool", "fixture.tool", "tool.json"), { id: "fixture.tool" });
  await json(path.join(roots.libraryRoot, "packages", "fixture.json"), {
    schema: "octopus.local_package", schema_version: 1, id: "fixture", version: "1.0.0", product_kind: "application", product_id: "world_fixture", release_status: "optional",
    display: { name: "Fixture", description: "Declared package ownership", developer: "Octopus", category: "provider" },
    members: [{ registry_kind: "interface", owner: "world_fixture", primary: true,
      configuration: [{ path: "interface.json", fields: [
        { pointer: "/config/model", schema: { type: "string", minLength: 1 } },
        { pointer: "/config/effort", schema: { type: "string", enum: ["medium", "high"] } },
      ] }] }, { registry_kind: "tool", owner: "fixture.tool" }],
  });
  return roots;
}
async function record(roots) { return (await readLocalPackageCatalog(roots)).packages.find((entry) => entry.id === "fixture"); }
function expected(record) {
  return { fixture: { sourceDigest: record.source_digest, installedDigest: record.installed_digest,
    receiptState: record.receipt_state, receiptDigest: record.receipt_digest || null } };
}
async function install(roots) {
  const current = await record(roots);
  return installLocalPackages({ ...roots, operationId: "fixture_install_001", packageIds: ["fixture"], expectedSourceDigests: { fixture: current.source_digest } });
}
async function repair(roots, operationId, onProgress) {
  return repairLocalPackages({ ...roots, operationId, packageIds: ["fixture"], expectedRepairs: expected(await record(roots)), onProgress });
}
function contentPin(body, mode) { return { sha256: crypto.createHash("sha256").update(body).digest("hex"), size: body.length, mode }; }
async function nativeRenameFixture(root) {
  await fs.mkdir(root, { recursive: true }); await fs.mkdir(path.join(root, "source"));
  await fs.writeFile(path.join(root, "source", "retained.bin"), "source bytes\n", { mode: 0o640 });
  const parent = await fs.open(root, fsSync.constants.O_RDONLY | fsSync.constants.O_DIRECTORY | fsSync.constants.O_NOFOLLOW);
  const rename = (from, to) => nativeFiles.renameDirectoryExclusive(parent.fd, from, parent.fd, to);
  const errno = number => error => error.code === "ERR_PACKAGE_RENAME" && error.errno === number;
  try {
    if (process.platform !== "darwin") {
      assert.throws(() => rename("source", "moved"), /require macOS with RENAME_EXCL support/);
      assert.equal(await fs.readFile(path.join(root, "source", "retained.bin"), "utf8"), "source bytes\n");
      return;
    }
    for (const name of ["", ".", "..", "../escape", "nested/target", "embedded\0nul", "x".repeat(256)]) {
      assert.throws(() => rename("source", name), errno(os.constants.errno.EINVAL));
    }
    await fs.mkdir(path.join(root, "target"));
    const targetInode = (await fs.stat(path.join(root, "target"))).ino;
    assert.throws(() => rename("source", "target"), errno(os.constants.errno.EEXIST));
    assert.equal((await fs.stat(path.join(root, "target"))).ino, targetInode); assert.deepEqual(await fs.readdir(path.join(root, "target")), []);
    await fs.writeFile(path.join(root, "file"), "unowned bytes\n");
    assert.throws(() => rename("source", "file"), errno(os.constants.errno.EEXIST));
    assert.throws(() => rename("file", "absent"), errno(os.constants.errno.EINVAL));
    await fs.symlink("source", path.join(root, "alias")); assert.throws(() => rename("alias", "absent"), errno(os.constants.errno.EINVAL));
    assert.throws(() => rename("missing", "absent"), errno(os.constants.errno.ENOENT));
    const file = await fs.open(path.join(root, "file"), "r");
    try { assert.throws(() => nativeFiles.renameDirectoryExclusive(file.fd, "source", parent.fd, "absent"), errno(os.constants.errno.EINVAL)); }
    finally { await file.close(); }
    rename("source", "moved");
    assert.equal(await fs.readFile(path.join(root, "moved", "retained.bin"), "utf8"), "source bytes\n");
    assert.equal((await fs.stat(path.join(root, "moved", "retained.bin"))).mode & 0o777, 0o640);
    assert.equal(await fs.readFile(path.join(root, "file"), "utf8"), "unowned bytes\n");
  } finally { await parent.close(); }
}

async function publicationRecoveryFixture(root) {
  for (const mode of ["prepare-verdict", "cleanup-move", "cleanup-reclaim"]) {
    const directory = path.join(root, mode), roots = await fixture(directory);
    child(mode, directory);
    await recoverLocalPackageTransactions(roots);
    assert.deepEqual(await retainedTransactionEntries(path.join(roots.stateRoot, "transactions")), []);
    await install(roots);
    assert.equal((await record(roots)).receipt_state, "valid");
    const temp = path.join(roots.stateRoot, "transactions", "..cleanup.abandoned_001.json.123.abcdef012345.tmp");
    await fs.writeFile(temp, "uncommitted", { mode: 0o600 });
    await recoverLocalPackageTransactions(roots);
    assert.equal(fsSync.existsSync(temp), false);
  }
  const roots = await fixture(path.join(root, "prepare-not-applied")), native = roots.publication;
  let destination;
  roots.publication = { ...native, async publishDirectoryMoves(operation, moves) {
    if (!destination && operation.includes(":prepare:")) {
      destination = moves[0].destination; await fs.mkdir(destination, { mode: 0o700 });
    }
    return native.publishDirectoryMoves(operation, moves);
  } };
  await assert.rejects(install(roots), error => error.publicationReceipts?.[0]?.outcome === "not_applied");
  assert.deepEqual(await fs.readdir(destination), []);
  await fs.rmdir(destination);
  await recoverLocalPackageTransactions(roots);
  await install(roots);
  assert.equal((await record(roots)).receipt_state, "valid");
}

async function crash(mode, root) {
  const roots = rootsFor(root), native = roots.publication;
  roots.publication = { ...native, async publishDirectoryMoves(operation, moves) {
    const result = await native.publishDirectoryMoves(operation, moves);
    if (moves.some(({ source: from, destination: to }) =>
      mode === "install-rename" && from.endsWith(".stage") && !path.basename(to).startsWith(".")
      || mode === "remove-rename" && to.endsWith(".backup")
      || mode === "remove-restore" && from.endsWith(".backup")
      || mode === "prepare-verdict" && path.basename(from).startsWith(".prepare.")
      || mode === "cleanup-move" && path.basename(to).startsWith(".cleanup."))) process.kill(process.pid, "SIGKILL");
    return result;
  }, async reclaimDirectory(file, receipt, expected) {
    await native.reclaimDirectory(file, receipt, expected);
    if (mode === "repair-cleanup" && file.endsWith(".backup")
      || mode === "cleanup-reclaim" && path.basename(file).startsWith(".cleanup.")) process.kill(process.pid, "SIGKILL");
  } };
  if (["install-rename", "prepare-verdict", "cleanup-move", "cleanup-reclaim"].includes(mode)) await install(roots);
  else if (mode === "remove-restore" || mode === "repair-cleanup") await recoverLocalPackageTransactions(roots);
  else if (mode === "remove-rename") await removeLocalPackages({ ...roots, operationId: "fixture_remove_001", packageIds: ["fixture"], expectedInstalledDigests: { fixture: (await record(roots)).installed_digest } });
  else if (mode === "repair-copy") await repair(roots, "fixture_partial_001", (progress) => {
    if (progress.phase === "copying") process.kill(process.pid, "SIGKILL");
  });
  else throw new Error(`unknown crash point ${mode}`);
  throw new Error(`crash point ${mode} was not reached`);
}
function child(mode, root) {
  const result = spawnSync(process.execPath, [__filename, "--crash", mode, root], { encoding: "utf8" });
  assert.equal(result.signal, "SIGKILL", result.stderr || result.stdout);
}
async function sourceBoundaryFixture(root, repoRoot) {
  for (const relative of [".gitignore", "LICENSE", "CONTRIBUTING.md", "README.md", "AGENTS.md",
    "docs/README.md", "docs/architecture.md", "docs/development.md", "docs/getting-started.md", "app/distribution/generated-outputs.json",
    "app/schemas/package_publication.schema.json", "app/schemas/client_extension_publication.schema.json",
    ...["check-public-source.cjs", "generated-source-boundary.cjs", "publication-lib.cjs", "export-public-package.cjs", "export-public-client-extension.cjs", "verify-publication.cjs"].map((name) => `app/scripts/${name}`)]) {
    await fs.mkdir(path.dirname(path.join(root, relative)), { recursive: true });
    await fs.copyFile(path.join(repoRoot, relative), path.join(root, relative));
  }
  await json(path.join(root, "registry", ".octopus-source.json"), { schema_version: 1, id: "fixture" });
  const output = readGeneratedSourcePaths(root).find((entry) => entry.endsWith("life-bin/"));
  const executable = path.join(root, output, "generated"), body = Buffer.from(`${os.homedir()}/build-host-path`);
  await fs.mkdir(path.dirname(executable), { recursive: true }); await fs.writeFile(executable, body);
  const check = (...args) => spawnSync(process.execPath, [path.join(root, "app", "scripts", "check-public-source.cjs"), ...args], { encoding: "utf8" });
  let result = check("--worktree"); assert.equal(result.status, 0, result.stderr);
  const unknown = path.join(root, "registry", "unrelated", "binary"); await fs.mkdir(path.dirname(unknown)); await fs.writeFile(unknown, body);
  result = check("--worktree"); assert.equal(result.status, 1); assert.match(result.stderr, /unrelated\/binary.*absolute user-home path/);
  await fs.unlink(unknown);
  execFileSync("git", ["init", "--quiet", root]); execFileSync("git", ["-C", root, "add", "--force", executable]);
  result = check(); assert.equal(result.status, 1); assert.match(result.stderr, /tracked generated build output/);
  assert.deepEqual(await fs.readFile(executable), body);
}
async function packageExportFixture(root, repoRoot) {
  for (const relative of ["LICENSE", "app/electron/localPackages.cjs", "app/scripts/export-public-package.cjs", "app/scripts/publication-lib.cjs",
    ...["package_publication", "local_package_manifest", "local_application_pack", "local_package_receipt", "local_package_transaction"].map((name) => `app/schemas/${name}.schema.json`)]) {
    await fs.mkdir(path.dirname(path.join(root, relative)), { recursive: true });
    await fs.copyFile(path.join(repoRoot, relative), path.join(root, relative));
  }
  const registry = path.join(root, "registry"), library = path.join(root, "app", "distribution", "local-packages"), out = path.join(root, "published");
  const descriptor = path.join(registry, "interface", "world_provider_codex", "interface.json");
  await json(path.join(registry, ".octopus-source.json"), { schema_version: 1, id: "fixture" });
  await json(descriptor, { id: "world_provider_codex" });
  await json(path.join(library, "packages", "fixture.json"), {
    schema: "octopus.local_package", schema_version: 1, id: "fixture", version: "1.0.0", product_kind: "application", product_id: "world_provider_codex", release_status: "optional",
    display: { name: "Fixture", description: "Selected package export", developer: "Octopus", category: "provider" },
    members: [{ registry_kind: "interface", owner: "world_provider_codex", primary: true }],
  });
  const body = Buffer.from(`${os.homedir()}/private-build-input`), unselected = path.join(registry, "unselected", "payload");
  await fs.mkdir(path.dirname(unselected)); await fs.writeFile(unselected, body);
  const publish = () => spawnSync(process.execPath, [path.join(root, "app", "scripts", "export-public-package.cjs"),
    "--package", "fixture", "--source-registry", registry, "--library-root", library, "--out", out,
    "--license", "Apache-2.0", "--license-file", path.join(root, "LICENSE")],
    { encoding: "utf8", env: { ...process.env, NODE_PATH: path.join(repoRoot, "app", "node_modules") } });
  let result = publish(); assert.equal(result.status, 0, result.stderr);
  const output = JSON.parse(result.stdout).output, publication = JSON.parse(await fs.readFile(path.join(output, "publication.json")));
  assert.deepEqual(publication.members[0].files.map((file) => file.path), ["registry/interface/world_provider_codex/interface.json"]);
  assert.deepEqual(await fs.readFile(path.join(output, "registry", "interface", "world_provider_codex", "interface.json")), await fs.readFile(descriptor));
  const generated = readGeneratedSourcePaths(repoRoot).find((entry) => entry.endsWith("life-bin/") && !entry.includes("/intake/"));
  const selectedBinary = path.join(root, generated, "generated");
  await fs.mkdir(path.dirname(selectedBinary), { recursive: true }); await fs.writeFile(selectedBinary, body);
  result = publish(); assert.equal(result.status, 1); assert.match(result.stderr, /absolute user-home path.*life-bin\/generated/);
  assert.deepEqual(await fs.readFile(selectedBinary), body);
  await fs.writeFile(selectedBinary, "reviewed executable bytes\n");
  await json(path.join(path.dirname(descriptor), "life.json"), { process: { bin: "target/debug/unreviewed" } });
  result = publish(); assert.equal(result.status, 1); assert.match(result.stderr, /development executable path.*life.json/);
  assert.deepEqual(await fs.readdir(out), [path.basename(output)]);
}
async function dataDirectoryFixture(root) {
  const roots = await fixture(root);
  const manifestPath = path.join(roots.libraryRoot, "packages", "fixture.json");
  const manifest = JSON.parse(await fs.readFile(manifestPath));
  manifest.members[0].data_directories = ["entries", "usage"];
  await json(manifestPath, manifest);
  await json(ownerFile(roots, false, "entries/shipped.json"), { id: "shipped", text: "one" });
  await install(roots);
  const runtime = {
    "entries/memory-001.json": `${JSON.stringify({ id: "memory-001", note: "remembered" })}\n`,
    "usage/memory-001.json": "{\"days\":[\"2026-11-16\"]}\n",
    "usage/nested/deep.bin": Buffer.from([0, 1, 2, 255]),
  };
  for (const [relative, body] of Object.entries(runtime)) {
    await fs.mkdir(path.dirname(ownerFile(roots, true, relative)), { recursive: true });
    await fs.writeFile(ownerFile(roots, true, relative), body, { mode: 0o600 });
  }
  let current = await record(roots);
  assert.equal(current.receipt_state, "valid");
  assert.deepEqual(current.local_changes, []);
  await assert.rejects(removeLocalPackages({ ...roots, operationId: "fixture_data_remove_001", packageIds: ["fixture"],
    expectedInstalledDigests: { fixture: current.installed_digest } }), /installation data|removal/);
  await json(ownerFile(roots, false, "entries/shipped.json"), { id: "shipped", text: "two" });
  current = await record(roots);
  assert.equal(current.update_available, true);
  assert.deepEqual(current.local_changes, []);
  await repair(roots, "fixture_data_repair_001");
  for (const [relative, body] of Object.entries(runtime)) {
    assert.deepEqual(await fs.readFile(ownerFile(roots, true, relative)), Buffer.from(body));
    assert.equal((await fs.stat(ownerFile(roots, true, relative))).mode & 0o777, 0o600);
  }
  assert.deepEqual(JSON.parse(await fs.readFile(ownerFile(roots, true, "entries/shipped.json"))), { id: "shipped", text: "two" });
  current = await record(roots);
  assert.equal(current.update_available, false);
  assert.deepEqual(current.local_changes, []);
  await json(ownerFile(roots, true, "entries/shipped.json"), { id: "shipped", text: "hand edited" });
  await json(ownerFile(roots, false, "entries/shipped.json"), { id: "shipped", text: "three" });
  current = await record(roots);
  assert.deepEqual(current.local_changes.map(change => [change.path, change.change]), [["entries/shipped.json", "modified"]]);
  await assert.rejects(repair(roots, "fixture_data_repair_002"), /local changes|unowned JSON changes/);
  assert.equal(JSON.parse(await fs.readFile(ownerFile(roots, true, "entries/shipped.json"))).text, "hand edited");
  await fs.rm(ownerFile(roots, true, "entries/shipped.json"));
  current = await record(roots);
  assert.deepEqual(current.local_changes.map(change => [change.path, change.change]), [["entries/shipped.json", "removed"]]);
  await assert.rejects(repair(roots, "fixture_data_repair_003"), /local changes/);
  assert.deepEqual(await fs.readFile(ownerFile(roots, true, "entries/memory-001.json")), Buffer.from(runtime["entries/memory-001.json"]));
}

async function run() {
  const root = await fs.realpath(await fs.mkdtemp(path.join(os.tmpdir(), "octopus-package-ownership-")));
  try {
    await publicationRecoveryFixture(path.join(root, "publication-recovery"));
    await nativeRenameFixture(path.join(root, "native-rename"));
    const roots = await fixture(path.join(root, "configuration"));
    await install(roots);
    await json(ownerFile(roots, true, "interface.json"), { id: "world_fixture", config: { model: "selected", endpoint: "configured" }, contract: 1 });
    let current = await record(roots);
    assert.equal(current.installed, true);
    assert.equal(current.receipt_state, "valid");
    assert.equal(current.customizations.length, 3);
    assert.notEqual(current.source_digest, current.installed_digest);
    const receiptPath = path.join(roots.stateRoot, "receipts", "fixture.json"), originalReceipt = await fs.readFile(receiptPath);
    const baseline = JSON.parse(originalReceipt), crossWired = structuredClone(baseline);
    crossWired.members.find((member) => member.owner === "world_fixture").digest = "a".repeat(64);
    await json(receiptPath, crossWired);
    assert.equal((await record(roots)).available, false);
    assert.match((await record(roots)).error, /materialized baseline does not match/);
    crossWired.members = structuredClone(baseline.members);
    crossWired.members.pop();
    await json(receiptPath, crossWired);
    assert.equal((await record(roots)).available, false);
    assert.match((await record(roots)).error, /source baseline does not match/);
    crossWired.members = [baseline.members[0], baseline.members[0]];
    await json(receiptPath, crossWired);
    assert.equal((await record(roots)).available, false);
    await fs.writeFile(receiptPath, originalReceipt);
    await fs.writeFile(ownerFile(roots, true, "life-bin"), "damaged\n");
    await json(ownerFile(roots, false, "interface.json"), { id: "world_fixture", config: { model: "new-default", effort: "high" }, contract: 2 });
    await fs.writeFile(ownerFile(roots, false, "life-bin"), "version two\n");
    current = await record(roots);
    assert.equal(current.receipt_state, "valid");
    const review = expected(current);
    await repairLocalPackages({ ...roots, operationId: "fixture_repair_001", packageIds: ["fixture"], expectedRepairs: review });
    assert.deepEqual(JSON.parse(await fs.readFile(ownerFile(roots, true, "interface.json"))), { id: "world_fixture", config: { model: "selected", endpoint: "configured" }, contract: 2 });
    assert.equal(await fs.readFile(ownerFile(roots, true, "life-bin"), "utf8"), "version two\n");
    current = await record(roots);
    assert.equal(current.installed, true);
    assert.equal(current.customized, true);
    assert.equal((await repairLocalPackages({ ...roots, operationId: "fixture_repair_001", packageIds: ["fixture"], expectedRepairs: review })).changed, false);
    await json(ownerFile(roots, true, "interface.json"), { id: "world_fixture", config: { model: "selected" }, contract: 999 });
    const unchanged = await fs.readFile(receiptPath);
    await assert.rejects(repair(roots, "fixture_refusal_001"), /unowned JSON changes/);
    assert.deepEqual(await fs.readFile(receiptPath), unchanged);
    assert.equal(JSON.parse(await fs.readFile(ownerFile(roots, true, "interface.json"))).contract, 999);
    await json(ownerFile(roots, true, "interface.json"), { id: "world_fixture", config: { model: 42 }, contract: 2 });
    assert.equal((await record(roots)).available, false);
    await json(ownerFile(roots, true, "interface.json"), { id: "world_fixture", config: { model: "another-selection" }, contract: 2 });
    current = await record(roots);
    assert.equal(current.receipt_state, "valid");
    await removeLocalPackages({ ...roots, operationId: "fixture_remove_002", packageIds: ["fixture"], expectedInstalledDigests: { fixture: current.installed_digest } });
    assert.equal((await record(roots)).materialized, false);
    for (const mode of ["install-rename", "remove-rename", "repair-copy"]) {
      const directory = path.join(root, mode), roots = await fixture(directory);
      if (mode !== "install-rename") await install(roots);
      if (mode === "repair-copy") await fs.writeFile(ownerFile(roots, false, "life-bin"), "version two\n");
      child(mode, directory);
      if (mode === "remove-rename") child("remove-restore", directory);
      const recovered = await record(roots);
      assert.equal(recovered.materialized, mode !== "install-rename");
      assert.equal(recovered.receipt_state, mode === "install-rename" ? "missing" : "valid");
      assert.deepEqual(await retainedTransactionEntries(path.join(roots.stateRoot, "transactions")), []);
    }
    const collision = await fixture(path.join(root, "foreign-target"));
    child("install-rename", path.join(root, "foreign-target"));
    const pending = JSON.parse(await fs.readFile(path.join(collision.stateRoot, "transactions", "fixture_install_001", "journal.json")));
    const foreignFile = path.join(collision.activeRegistryRoot, pending.moves[0].target, "tool.json");
    await fs.writeFile(foreignFile, "foreign bytes\n");
    await assert.rejects(recoverLocalPackageTransactions(collision), error => {
      assert.equal(error.code, "client_publication_failed");
      assert.match(error.message, /predecessor or destination differs from the exact requested manifest/);
      assert.equal(error.publicationReceipts.length, 1);
      assert.equal(error.publicationReceipts[0].outcome, "not_applied");
      assert.equal(error.publicationReceipts[0].movement.source, path.dirname(foreignFile));
      return true;
    });
    assert.equal(await fs.readFile(foreignFile, "utf8"), "foreign bytes\n");
    const staging = await fixture(path.join(root, "foreign-stage"));
    const foreignStage = path.join(staging.activeRegistryRoot, "tool", ".fixture.tool.fixture_install_001.stage", "foreign.json");
    await json(foreignStage, { retained: true });
    await assert.rejects(install(staging), /package target or transaction path already exists/);
    assert.deepEqual(JSON.parse(await fs.readFile(foreignStage)), { retained: true });
    const metadata = await fixture(path.join(root, "metadata"));
    const manifestPath = path.join(metadata.libraryRoot, "packages", "fixture.json");
    const manifest = JSON.parse(await fs.readFile(manifestPath));
    for (const pointer of ["/config/~2", "/id"]) {
      const invalid = structuredClone(manifest); invalid.members[0].configuration[0].fields[0].pointer = pointer;
      await json(manifestPath, invalid);
      await assert.rejects(readLocalPackageCatalog(metadata), /invalid explicit manifests/);
    }
    for (const directories of [[], [""], ["."], [".."], ["/entries"], ["C:/entries"], ["C:entries"],
      ["entries/"], ["./entries"], ["entries/../other"], ["entries\\other"], ["entries//other"],
      ["entries/\0other"], ["entries/target"], ["entries/.git"], ["entries", "entries"], ["entries", "entries/nested"]]) {
      const invalid = structuredClone(manifest); invalid.members[0].data_directories = directories;
      await json(manifestPath, invalid);
      await assert.rejects(readLocalPackageCatalog(metadata), /invalid explicit manifests/);
    }
    const dataManifest = structuredClone(manifest); dataManifest.members[0].data_directories = ["entries", "entry"];
    await json(manifestPath, dataManifest);
    assert.equal((await record(metadata)).available, true);
    dataManifest.members[0].data_directories = ["interface.json"];
    await json(manifestPath, dataManifest);
    assert.match((await record(metadata)).error, /data directory is a file/);
    dataManifest.members[0].data_directories = ["entries"];
    dataManifest.members[0].configuration[0].path = "entries/settings.json";
    await json(manifestPath, dataManifest);
    await assert.rejects(readLocalPackageCatalog(metadata), /overlaps configuration ownership/);
    await json(manifestPath, manifest);
    const before = await record(metadata);
    await fs.writeFile(ownerFile(metadata, false, "life-bin"), "version two\n");
    assert.notEqual((await record(metadata)).source_digest, before.source_digest);
    let changed = false;
    await assert.rejects(installLocalPackages({ ...metadata, operationId: "manifest_race_001", packageIds: ["fixture"], expectedSourceDigests: { fixture: (await record(metadata)).source_digest },
      onProgress(progress) { if (progress.phase === "copying" && !changed) { changed = true; fsSync.writeFileSync(manifestPath, JSON.stringify({ ...manifest, version: "2.0.0" })); } } }), /definition changed/);
    assert.deepEqual(await retainedTransactionEntries(path.join(metadata.stateRoot, "transactions")), []);
    const alias = await fixture(path.join(root, "alias")), outside = path.join(root, "outside");
    await fs.mkdir(alias.activeRegistryRoot, { recursive: true }); await fs.mkdir(outside);
    await fs.symlink(outside, path.join(alias.activeRegistryRoot, "tool"));
    await assert.rejects(install(alias), /not a plain directory/);
    assert.deepEqual(await fs.readdir(outside), []);
    const repoRoot = path.resolve(__dirname, "../.."), generated = readGeneratedSourcePaths(repoRoot);
    const ignored = new Set(fsSync.readFileSync(path.join(repoRoot, ".gitignore"), "utf8").split(/\r?\n/));
    for (const output of generated) {
      assert.ok(ignored.has(`/${output}`));
      assert.equal(isGeneratedSourcePath(output, generated), true);
      assertDeclaredGeneratedOutput(path.join(repoRoot, output), repoRoot, generated);
      assert.throws(() => validatePublicPayload(output, Buffer.from(`${os.homedir()}/private-build-input`), { privateRoots: [] }), /absolute user-home path/);
    }
    assert.equal(isGeneratedSourcePath("registry/unrelated/bin/payload", generated), false);
    assert.equal(isGeneratedSourcePath(`${generated[0]}../../unexpected`, generated), false);
    assert.throws(() => assertDeclaredGeneratedOutput(path.join(repoRoot, "registry", "unrelated", "payload"), repoRoot, generated), /not declared/);
    assert.throws(() => validatePublicPayload("life.json", Buffer.from('{"bin":"target/debug/unreviewed"}'), { privateRoots: [], rejectDevelopmentExecutables: true }), /development executable path/);
    await dataDirectoryFixture(path.join(root, "data-directories"));
    await sourceBoundaryFixture(path.join(root, "source-boundary"), repoRoot);
    await packageExportFixture(path.join(root, "package-export"), repoRoot);
    process.stdout.write("package configuration ownership and rename-window recovery passed\n");
  } finally { await fs.rm(root, { recursive: true, force: true }); }
}
if (require.main === module) {
  const focused = { "--publication-recovery": publicationRecoveryFixture, "--data-directories": dataDirectoryFixture }[process.argv[2]];
  const action = process.argv[2] === "--crash" ? crash(process.argv[3], process.argv[4])
    : focused ? (async () => {
      const root = await fs.realpath(await fs.mkdtemp(path.join(os.tmpdir(), "octopus-package-contract-")));
      try { await focused(root); }
      finally { await fs.rm(root, { recursive: true, force: true }); }
    })() : run();
  action.catch((error) => {
    process.stderr.write(`${error.stack || error}\n`); process.exitCode = 1;
  });
}
module.exports = { runPackageOwnership: run };
