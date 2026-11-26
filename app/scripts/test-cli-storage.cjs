const assert = require("node:assert/strict");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const { spawnSync } = require("node:child_process");
const { test } = require("node:test");
const { inspectStorage, resolveStorageHome } = require("../cli/storage.cjs");
const cli = path.resolve(__dirname, "../cli/index.cjs");
function fixture(t) {
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "octopus-storage-")));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  return root;
}
function write(home, relative, content) {
  const file = path.join(home, relative);
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, content);
}
function invoke(home, args = [], env = {}) {
  return spawnSync(process.execPath, [cli, "storage", "--home", home, ...args], {
    encoding: "utf8", env: { ...process.env, ...env, OCTOPUS_RUNTIME_BIN: "/unavailable-runtime", OCTOPUS_APP_LISTEN: "invalid" },
  });
}
function snapshot(root) {
  const values = {};
  const visit = file => {
    const stat = fs.lstatSync(file, { bigint: true });
    values[file] = [stat.size.toString(), stat.mtimeNs.toString(), stat.mode.toString(),
      stat.isFile() ? fs.readFileSync(file).toString("hex") : null];
    if (stat.isDirectory()) for (const name of fs.readdirSync(file)) visit(path.join(file, name));
  };
  visit(root);
  return values;
}
function independentTotals(root) {
  let logical = 0n, allocated = 0n, files = 0, directories = 0, symlinks = 0, special = 0;
  const visit = file => {
    const stat = fs.lstatSync(file, { bigint: true });
    logical += stat.size; allocated += stat.blocks * 512n;
    if (stat.isDirectory()) { directories++; for (const name of fs.readdirSync(file)) visit(path.join(file, name)); }
    else if (stat.isSymbolicLink()) symlinks++;
    else if (stat.isFile()) files++;
    else special++;
  };
  visit(root);
  return { logical_bytes: String(logical), allocated_bytes: String(allocated), files, directories, symlinks, special };
}
test("storage normal fixture: exact POSIX sizes, generic interface breakdown, top five and unchanged files", async t => {
  const home = fixture(t);
  for (const name of ["movements.jsonl", "ledger-index.sqlite", "ledger-index.sqlite-wal", "ledger-index.sqlite-shm", "blobs/a", "metadata/a"]) write(home, "ledger/" + name, name);
  write(home, "interfaces/interface_alpha/state/store.sqlite", "alpha data");
  write(home, "interfaces/interface_alpha/outbox/one", "outbox");
  write(home, "interfaces/interface_beta/cache/v1/payload", "cached payload bytes");
  write(home, "interfaces/interface_beta/cache/v2/payload", "newer payload");
  const amendment = { before: "é".repeat(30), after: "next", effect: { payload: { data: "payload" } } };
  for (let i = 0; i < 7; i++) write(home, "state/runs/run" + i + ".json",
    JSON.stringify({ interface_state: { owner: { capability_amendment: amendment } }, filler: "x".repeat(i * 100) }, null, 2));
  const fd = fs.openSync(path.join(home, "sparse"), "w"); fs.ftruncateSync(fd, 4 * 1024 * 1024); fs.closeSync(fd);
  const outside = fixture(t);
  write(outside, "large", "x".repeat(10000));
  fs.symlinkSync(outside, path.join(home, "external"));
  fs.symlinkSync(home, path.join(home, "cycle"));
  fs.linkSync(path.join(home, "ledger/blobs/a"), path.join(home, "alias"));
  const before = snapshot(home);
  const result = invoke(home, ["--json"]);
  assert.equal(result.status, 0, result.stderr + result.stdout);
  const report = JSON.parse(result.stdout);
  assert.equal(report.schema_version, 1);
  assert.equal(report.ok, true);
  assert.deepEqual(report.errors, []);
  assert.deepEqual(report.total, independentTotals(home));
  assert.equal(report.ledger.movements.logical_bytes, String(Buffer.byteLength("movements.jsonl")));
  assert.equal(report.ledger.wal.files, 1);
  assert.equal(report.interfaces.count, 2);
  for (const entry of report.interfaces.entries) {
    assert.deepEqual(Object.keys(entry), ["id", "total", "top_directories"]);
  }
  const alpha = report.interfaces.entries.find(x => x.id === "interface_alpha");
  assert.equal(alpha.total.files, 2);
  assert.ok(alpha.top_directories.some(d => d.path === "interfaces/interface_alpha/state" && d.files === 1));
  const beta = report.interfaces.entries.find(x => x.id === "interface_beta");
  assert.equal(beta.total.files, 2);
  assert.ok(beta.top_directories.some(d => d.path === "interfaces/interface_beta/cache" && d.files === 2));
  assert.equal(report.runs.count, 7);
  assert.equal(report.runs.top.length, 5);
  assert.ok(report.runs.top[0].path.endsWith("run6.json"));
  assert.equal(report.runs.top[0].inline_amendment_bytes, String(Buffer.byteLength(JSON.stringify(amendment))));
  assert.equal(report.runs.top[0].inline_amendment_payload_bytes, String(Buffer.byteLength(JSON.stringify(amendment.effect.payload))));
  assert.deepEqual(snapshot(home), before);
  assert.match(invoke(home).stdout, /Ledger wal:.*inline amendment/s);
  assert.deepEqual(await inspectStorage(home), report);
});
test("empty and missing Home remain offline and do not create state", async t => {
  const root = fixture(t), missing = path.join(root, "missing");
  const empty = JSON.parse(invoke(root, ["--json"]).stdout);
  assert.deepEqual(empty.total, independentTotals(root));
  assert.equal(empty.runs.count, 0);
  assert.equal(empty.interfaces.count, 0);
  const report = JSON.parse(invoke(missing, ["--json"]).stdout);
  assert.equal(report.exists, false);
  assert.equal(report.total.logical_bytes, "0");
  assert.equal(report.ok, true);
  assert.deepEqual(fs.readdirSync(root), []);
});
test("long Home/TMPDIR bypass sockets, runtime and client build prerequisites", t => {
  const root = fixture(t), home = path.join(root, "a".repeat(90), "b".repeat(90));
  fs.mkdirSync(home, { recursive: true });
  const result = invoke(home, ["--json"], { TMPDIR: home });
  assert.equal(result.status, 0, result.stderr);
  assert.equal(JSON.parse(result.stdout).home, home);
  assert.deepEqual(fs.readdirSync(home), []);
});
test("errors and symlink Home are explicit and never follow targets", async t => {
  const root = fixture(t), home = path.join(root, "home");
  write(home, "state/runs/bad.json", "{malformed");
  const result = invoke(home, ["--json"]);
  assert.equal(result.status, 1);
  const report = JSON.parse(result.stdout);
  assert.equal(report.ok, false);
  assert.equal(report.runs.top[0].inline_amendment_bytes, null);
  assert.equal(report.errors[0].operation, "read_run");
  fs.symlinkSync(home, path.join(root, "link"));
  const linked = await inspectStorage(path.join(root, "link"));
  assert.equal(linked.errors[0].code, "ELOOP");
  assert.equal(linked.total.files, 0);
  const throughLink = await inspectStorage(path.join(root, "link", "state"));
  assert.equal(throughLink.ok, true);
  assert.equal(throughLink.total.files, 1);
});
test("schema uses exact decimal bytes, stable keys, and CLI help/argument validation", t => {
  const home = fixture(t);
  const report = JSON.parse(invoke(home, ["--json"]).stdout);
  assert.deepEqual(Object.keys(report), ["schema_version", "home", "exists", "ok", "measurement", "total", "top_directories", "ledger", "interfaces", "runs", "errors"]);
  const checkSize = node => {
    assert.match(node.logical_bytes, /^\d+$/); assert.match(node.allocated_bytes, /^\d+$/);
    for (const key of ["files", "directories", "symlinks", "special"]) assert.ok(Number.isSafeInteger(node[key]) && node[key] >= 0);
  };
  checkSize(report.total);
  for (const size of Object.values(report.ledger)) checkSize(size);
  assert.match(invoke(home, ["--help"]).stdout, /octopus storage.*Read-only offline/s);
  assert.equal(invoke(home, ["unexpected", "--json"]).status, 1);
  assert.equal(invoke(home, ["prune", "--json"]).status, 1);
  assert.match(spawnSync(process.execPath, [cli, "--help"], { encoding: "utf8" }).stdout, /octopus storage/);
  assert.equal(resolveStorageHome("~/test", {}, "/", "/users/test"), "/users/test/test");
  assert.equal(resolveStorageHome(undefined, { OCTOPUS_HOME: home }), home);
});

test("oversized run and unreadable directory produce partial errors, not invented success", async t => {
  const home = fixture(t);
  write(home, "state/runs/large.json", "");
  fs.truncateSync(path.join(home, "state/runs/large.json"), 257 * 1024 * 1024);
  let report = await inspectStorage(home);
  assert.equal(report.ok, false);
  assert.equal(report.errors[0].code, "EFBIG");
  assert.equal(report.runs.top[0].logical_bytes, String(257 * 1024 * 1024));
  assert.equal(report.runs.top[0].inline_amendment_bytes, null);
  fs.unlinkSync(path.join(home, "state/runs/large.json"));
  if (process.getuid?.() === 0) return;
  const denied = path.join(home, "denied");
  fs.mkdirSync(denied);
  fs.chmodSync(denied, 0);
  try {
    report = await inspectStorage(home);
    assert.equal(report.ok, false);
    assert.ok(report.errors.some(error => error.operation === "readdir" && error.code === "EACCES"));
  } finally { fs.chmodSync(denied, 0o700); }
});

test("offline storage accepts a symlinked ancestor without following Home entries", async t => {
  const root = fixture(t);
  fs.mkdirSync(path.join(root, "physical", "home"), { recursive: true });
  fs.symlinkSync(path.join(root, "physical"), path.join(root, "alias"), "dir");
  const physical = JSON.parse(invoke(path.join(root, "physical", "home"), ["--json"]).stdout);
  const result = invoke(path.join(root, "alias", "home"), ["--json"]);
  assert.equal(result.status, 0, result.stderr + result.stdout);
  assert.deepEqual(JSON.parse(result.stdout).total, physical.total);
});
