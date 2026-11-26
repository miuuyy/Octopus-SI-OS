const assert = require("node:assert/strict");
const fs = require("node:fs");
const fsp = require("node:fs/promises");
const os = require("node:os");
const path = require("node:path");
const { test } = require("node:test");
const { runClientDoctor } = require("../electron/clientDoctor.cjs");

function fixture(t) {
  const home = fs.mkdtempSync(path.join(os.tmpdir(), "doctor-ledger-"));
  t.after(() => fs.rmSync(home, { recursive: true, force: true }));
  const runs = path.join(home, "runs");
  const ledger = path.join(home, "ledger/movements.jsonl");
  fs.mkdirSync(runs);
  fs.mkdirSync(path.dirname(ledger));
  fs.writeFileSync(path.join(home, "host.json"), JSON.stringify({
    host_manifest_version: 1, root_refs: { run_records_root: runs },
  }));
  for (const id of ["run_observed", "run_mark", "run_ghost"]) {
    fs.writeFileSync(path.join(runs, id + ".json"), JSON.stringify({ id, status: "running" }));
  }
  return {
    ledger,
    doctor: () => runClientDoctor({
      roots: { activeRegistryRoot: path.join(home, "registry"), sourceRegistryRoot: path.join(home, "source"),
        libraryRoot: path.join(home, "library"), stateRoot: path.join(home, "state") },
      inspectInstallation: async () => null,
      inspectCatalog: async () => ({ packages: [], errors: [] }),
    }),
  };
}

test("doctor streams a ledger larger than its chunks without readFile, preserving run evidence", async (t) => {
  const { ledger, doctor } = fixture(t);
  const row = JSON.stringify({ kind: "InterfaceOutbound", invocation_metadata: { runtime: { run: "run_observed" } },
    padding: "é".repeat(40000) }) + "\n";
  const fd = fs.openSync(ledger, "w");
  for (let i = 0; i < 128; i++) fs.writeSync(fd, row);
  fs.writeSync(fd, '\n' + JSON.stringify({ kind: { MarkOutcomeAdmitted: {} }, invocation_metadata: { run: "run_mark" } }));
  fs.closeSync(fd);
  const original = fsp.readFile;
  t.mock.method(fsp, "readFile", (...args) => {
    assert.notEqual(args[0], ledger, "ledger must never be read into a single string");
    return original(...args);
  });
  const before = fs.statSync(ledger);
  const result = await doctor();
  assert.deepEqual(result.findings.filter(f => f.code.startsWith("run.")).map(f => [f.code, f.subject]),
    [["run.running_without_ledger_movement", "run_ghost"]]);
  assert.equal(fs.statSync(ledger).mtimeMs, before.mtimeMs);
});

test("doctor validates malformed trailing rows even after every running run is observed", async (t) => {
  const { ledger, doctor } = fixture(t);
  const rows = ["run_observed", "run_mark", "run_ghost"].map(run =>
    JSON.stringify({ invocation_metadata: { runtime: { run } } }));
  for (const bad of ["not-json", " ", "\r"]) {
    fs.writeFileSync(ledger, rows.join("\n") + "\n\n" + bad + "\n");
    await assert.rejects(doctor(), /doctor ledger line 5 is malformed/);
  }
});

test("doctor distinguishes an unavailable ledger from an empty ledger", async (t) => {
  const { ledger, doctor } = fixture(t);
  assert.equal((await doctor()).findings.filter(f => f.code === "run.ledger_unavailable").length, 1);
  fs.writeFileSync(ledger, "");
  assert.equal((await doctor()).findings.filter(f => f.code === "run.running_without_ledger_movement").length, 3);
});
