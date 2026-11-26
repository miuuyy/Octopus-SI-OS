const test = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const os = require("node:os");
const crypto = require("node:crypto");
const { spawnSync } = require("node:child_process");
const { inspectLocalPackageCatalog, inspectLocalPackageDrift, installLocalPackages } = require("../electron/localPackages.cjs");
const { fixtureDirectoryPublisher, fixtureFilePublisher } = require("./fixturePublication.cjs");
const { checkPackages } = require("../cli/packages.cjs");

const ids = ["driver.desktop_session", "artifact.codex_context"];
const mark = { registry_kind: "mark", owner: "task.summary.received" };
const json = (file, value) => { fs.mkdirSync(path.dirname(file), { recursive: true }); fs.writeFileSync(file, JSON.stringify(value)); };
const inventory = root => {
  const rows = [];
  const visit = dir => {
    for (const entry of fs.readdirSync(dir, { withFileTypes: true }).sort((a, b) => a.name.localeCompare(b.name))) {
      const file = path.join(dir, entry.name), stat = fs.lstatSync(file);
      rows.push([path.relative(root, file), stat.mode, entry.isDirectory() ? null :
        crypto.createHash("sha256").update(entry.isSymbolicLink() ? fs.readlinkSync(file) : fs.readFileSync(file)).digest("hex")]);
      if (entry.isDirectory()) visit(file);
    }
  };
  visit(root); return rows;
};
async function fixture(run, { longHome = false } = {}) {
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "octopus-drift-")));
  const home = path.join(root, longHome ? "home-" + "x".repeat(140) : "home"), libraryRoot = path.join(root, "library");
  const roots = { sourceRegistryRoot: path.join(libraryRoot, "registry"), activeRegistryRoot: path.join(home, "registry"),
    libraryRoot, stateRoot: path.join(home, "client/package-state"),
    publication: { ...fixtureDirectoryPublisher(home), publishFiles: fixtureFilePublisher(home) } };
  fs.mkdirSync(roots.activeRegistryRoot, { recursive: true });
  const manifests = ids.map(id => ({ schema: "octopus.local_package", schema_version: 1, id, version: "1.0.0",
    product_kind: "application", product_id: id, release_status: "optional",
    display: { name: id, description: "Drift fixture", developer: "Octopus", category: "sensor" },
    members: [{ registry_kind: "interface", owner: id, primary: true, data_directories: ["data"],
      configuration: [{ path: "interface.json", fields: [{ pointer: "/display/name", schema: { type: "string" } }] }] }] }));
  manifests[0].members.push(mark);
  const save = () => manifests.forEach(m => json(path.join(libraryRoot, "packages", m.id + ".json"), m));
  ids.forEach(id => json(path.join(roots.sourceRegistryRoot, "interface", id, "interface.json"),
    { id, interface_type: "world", role: "sensor", direction: "inbound", source_names: [id], display: { name: id } }));
  json(path.join(roots.sourceRegistryRoot, "mark/task.summary.received/mark.json"),
    { id: mark.owner, package: mark.owner, class: "success" });
  save();
  try {
    const catalog = await inspectLocalPackageCatalog(roots);
    await installLocalPackages({ ...roots, operationId: "drift_fixture", packageIds: ids,
      expectedSourceDigests: Object.fromEntries(catalog.packages.map(p => [p.id, p.source_digest])) });
    await run({ root, home, roots, manifests, save });
  } finally { fs.rmSync(root, { recursive: true, force: true }); }
}
function cli(f, args = ["--json"]) {
  return spawnSync(process.execPath, [path.join(__dirname, "../cli/index.cjs"), "packages", "check", ...args, "--home", f.home],
    { encoding: "utf8", env: { ...process.env, OCTOPUS_INSTALLATION_ROOT: path.join(f.root, "runtime"),
      OCTOPUS_LOCAL_PACKAGE_REGISTRY: f.roots.sourceRegistryRoot, OCTOPUS_LOCAL_PACKAGE_LIBRARY: f.roots.libraryRoot } });
}
async function observe(f) {
  const before = inventory(f.root);
  const report = await inspectLocalPackageDrift(f.roots);
  assert.deepEqual(inventory(f.root), before, "inspection must not change source or Home files, modes or directories");
  assert.deepEqual(JSON.parse(JSON.stringify(report)), report);
  assert.deepEqual(Object.keys(report), ["schema_version", "ok", "packages", "transfers", "issues"]);
  for (const pkg of report.packages) assert.deepEqual(Object.keys(pkg), ["id", "receipt_state", "update_available", "files", "issues"]);
  assert.equal(report.schema_version, 1);
  return report;
}
test("clean receipts are deterministic and CLI check is read-only with exit 0", () => fixture(async f => {
  const report = await observe(f); assert.equal(report.ok, true);
  assert.deepEqual(await observe(f), report);
  let text = ""; assert.equal(await checkPackages(f.roots, false, { write: s => { text += s; } }), 0);
  assert.match(text, /Packages: clean/);
  const before = inventory(f.root);
  const result = cli(f);
  assert.equal(result.status, 0, result.stderr + result.stdout);
  assert.deepEqual(JSON.parse(result.stdout), report);
  assert.deepEqual(inventory(f.root), before);
}));
test("offline check accepts a long Home while daemon environments keep their socket guard", () => fixture(async f => {
  assert.ok(Buffer.byteLength(f.home) > 107, "Home exceeds both macOS and Linux socket limits");
  const before = inventory(f.root), result = cli(f);
  assert.equal(result.status, 0, result.stderr + result.stdout);
  assert.deepEqual(JSON.parse(result.stdout), await observe(f));
  const { resolveEnvironment, resolvePackageRoots, createEnvironment } = require("../cli/environment.cjs");
  const roots = resolvePackageRoots({ home: f.home });
  assert.equal(roots.activeRegistryRoot, f.roots.activeRegistryRoot);
  assert.equal(Object.hasOwn(roots, "publication"), false);
  assert.throws(() => resolveEnvironment({ home: f.home }), /too long for a Unix socket/);
  assert.throws(() => createEnvironment({ home: f.home }), /too long for a Unix socket/);
  const daemon = spawnSync(process.execPath, [path.join(__dirname, "../cli/index.cjs"), "status", "--home", f.home, "--json"], { encoding: "utf8" });
  assert.equal(daemon.status, 1);
  assert.match(JSON.parse(daemon.stdout).message, /too long for a Unix socket/);
  assert.deepEqual(inventory(f.root), before);
}, { longHome: true }));
for (const action of ["modified", "missing", "mode_changed"]) test(action + " managed descriptor reports exact owner/path and exit 1", () => fixture(async f => {
  const file = path.join(f.roots.activeRegistryRoot, "interface", ids[0], "interface.json");
  if (action === "modified") { const d = JSON.parse(fs.readFileSync(file)); d.direction = "outbound"; json(file, d); }
  if (action === "missing") fs.unlinkSync(file);
  if (action === "mode_changed") fs.chmodSync(file, 0o700);
  const report = await observe(f); assert.equal(report.ok, false);
  assert.deepEqual(report.packages.find(p => p.id === ids[0]).files,
    [{ registry_kind: "interface", owner: ids[0], path: "interface.json", reason: action }]);
  let output = ""; assert.equal(await checkPackages(f.roots, true, { write: s => { output += s; } }), 1);
  assert.deepEqual(JSON.parse(output), report);
  const before = inventory(f.root), result = cli(f);
  assert.equal(result.status, 1, result.stderr);
  assert.deepEqual(JSON.parse(result.stdout), report);
  assert.deepEqual(inventory(f.root), before);
}));
test("declared mutable data and allowed configuration are not managed drift", () => fixture(async f => {
  json(path.join(f.roots.activeRegistryRoot, "interface", ids[0], "data/runtime.json"), { private: "not-for-output" });
  const file = path.join(f.roots.activeRegistryRoot, "interface", ids[0], "interface.json"), d = JSON.parse(fs.readFileSync(file));
  d.display.name = "local preference"; json(file, d);
  const report = await observe(f); assert.equal(report.ok, true);
  assert.ok(!JSON.stringify(report).includes("not-for-output"));
}));
for (const drifted of [false, true]) test("ownership transfer preview " + (drifted ? "blocked by drift" : "ready"), () => fixture(async f => {
  f.manifests[0].members.pop(); f.manifests[1].members.push(mark); f.save();
  if (drifted) json(path.join(f.roots.activeRegistryRoot, "mark/task.summary.received/mark.json"),
    { id: mark.owner, package: mark.owner, class: "failure" });
  const report = await observe(f); assert.equal(report.ok, !drifted);
  assert.deepEqual(report.transfers, [{ ...mark, from: ids[0], to: ids[1], state: drifted ? "blocked" : "ready",
    reason: drifted ? "drift_or_issue" : "ownership_transfer" }]);
  assert.ok(report.packages.every(p => p.update_available));
}));
test("duplicate target ownership is a structured conflict", () => fixture(async f => {
  f.manifests[1].members.push(mark); f.save();
  const report = await observe(f); assert.equal(report.ok, false);
  assert.deepEqual(report.issues, [{ code: "duplicate_target_owner", member: "mark/task.summary.received" }]);
}));
test("invalid pack metadata is reported without exposing catalog contents", () => fixture(async f => {
  json(path.join(f.roots.libraryRoot, "packs/broken.json"), { private: "not-for-output" });
  const report = await observe(f); assert.equal(report.ok, false);
  assert.deepEqual(report.issues, [{ code: "catalog_invalid", member: null }]);
  assert.ok(!JSON.stringify(report).includes("not-for-output"));
}));
test("source-only updates are not local drift, while unowned added files require review", () => fixture(async f => {
  json(path.join(f.roots.sourceRegistryRoot, "interface", ids[0], "new.json"), { update: true });
  let report = await observe(f); assert.equal(report.ok, true);
  assert.equal(report.packages.find(p => p.id === ids[0]).update_available, true);
  json(path.join(f.roots.activeRegistryRoot, "interface", ids[0], "extra.json"), { local: true });
  report = await observe(f); assert.equal(report.ok, false);
  assert.deepEqual(report.packages.find(p => p.id === ids[0]).files,
    [{ registry_kind: "interface", owner: ids[0], path: "extra.json", reason: "added" }]);
}));
test("invalid receipt and pending transaction are reported, not recovered", () => fixture(async f => {
  fs.writeFileSync(path.join(f.roots.stateRoot, "receipts", ids[0] + ".json"), "{");
  fs.mkdirSync(path.join(f.roots.stateRoot, "transactions", "pending"));
  const report = await observe(f); assert.equal(report.ok, false);
  assert.ok(report.issues.some(i => i.code === "pending_transaction"));
  assert.equal(report.packages.find(p => p.id === ids[0]).receipt_state, "invalid");
}));
test("publication lock metadata alone is not an unfinished package transaction", () => fixture(async f => {
  const metadata = path.join(f.roots.stateRoot, "transactions/.octopus-publication");
  fs.mkdirSync(metadata, { recursive: true, mode: 0o700 });
  fs.writeFileSync(path.join(metadata, "dir-fixture.lock"), "", { mode: 0o600 });
  const before = inventory(f.root), report = await observe(f), result = cli(f);
  assert.equal(report.ok, true);
  assert.equal(result.status, 0, result.stdout + result.stderr);
  assert.deepEqual(JSON.parse(result.stdout), report);
  assert.deepEqual(inventory(f.root), before);
}));
for (const entry of ["pending", ".prepare.fixture.json", ".cleanup.fixture.json"]) test("real pending transaction stays fail-closed: " + entry, () => fixture(async f => {
  const directory = path.join(f.roots.stateRoot, "transactions");
  fs.mkdirSync(path.join(directory, ".octopus-publication"), { recursive: true, mode: 0o700 });
  if (entry === "pending") fs.mkdirSync(path.join(directory, entry));
  else fs.writeFileSync(path.join(directory, entry), "{}");
  const report = await observe(f), result = cli(f);
  assert.equal(report.ok, false); assert.equal(result.status, 1);
  assert.ok(report.issues.some(issue => issue.code === "pending_transaction"));
}));
for (const type of ["file", "symlink", "world-readable"]) test("invalid publication metadata is not hidden: " + type, () => fixture(async f => {
  const metadata = path.join(f.roots.stateRoot, "transactions/.octopus-publication");
  if (type === "file") fs.writeFileSync(metadata, "");
  else if (type === "symlink") fs.symlinkSync(f.root, metadata);
  else fs.mkdirSync(metadata, { mode: 0o755 });
  const report = await observe(f);
  assert.equal(report.ok, false);
  assert.ok(report.issues.some(issue => issue.code === "invalid_transaction_state"));
}));
test("updater packages check reuses JSON report without constructing a publisher", () => fixture(async f => {
  const app = path.join(f.root, "build/Octopus.app/Contents/Resources/app");
  fs.mkdirSync(path.join(app, "electron"), { recursive: true });
  fs.cpSync(f.roots.libraryRoot, path.join(app, "local-packages"), { recursive: true });
  fs.writeFileSync(path.join(app, "electron/localPackages.cjs"), `module.exports = require(${JSON.stringify(path.resolve(__dirname, "../electron/localPackages.cjs"))})`);
  fs.writeFileSync(path.join(app, "electron/packageStorage.cjs"), 'throw new Error("Read-only check loaded a write publisher")');
  const metadata = path.join(f.roots.stateRoot, "transactions/.octopus-publication");
  fs.mkdirSync(metadata, { recursive: true, mode: 0o700 });
  fs.writeFileSync(path.join(metadata, "dir-fixture.lock"), "", { mode: 0o600 });
  const args = [path.join(__dirname, "safe-self-update.cjs"), "packages", "check", "--json", "--target", "app",
    "--build-root", path.join(f.root, "build"), "--home", f.home];
  let before = inventory(f.root);
  let result = spawnSync(process.execPath, args, { encoding: "utf8" });
  assert.equal(result.status, 0, result.stdout + result.stderr);
  assert.equal(JSON.parse(result.stdout).schema_version, 1);
  assert.deepEqual(inventory(f.root), before);
  fs.unlinkSync(path.join(f.roots.activeRegistryRoot, "mark/task.summary.received/mark.json"));
  before = inventory(f.root); result = spawnSync(process.execPath, args, { encoding: "utf8" });
  assert.equal(result.status, 1, result.stdout + result.stderr);
  assert.equal(JSON.parse(result.stdout).ok, false);
  assert.deepEqual(inventory(f.root), before);
}));
test("unreceipted target owner and removed target members require review", () => fixture(async f => {
  fs.unlinkSync(path.join(f.roots.stateRoot, "receipts", ids[1] + ".json"));
  f.manifests[0].members.pop(); f.save();
  const report = await observe(f); assert.equal(report.ok, false);
  assert.ok(report.packages.some(p => p.receipt_state === "missing" && p.issues.some(i => i.code === "unreceipted_owner")));
  assert.ok(report.packages.find(p => p.id === ids[0]).issues.some(i => i.code === "owner_removal_requires_review"));
}));
