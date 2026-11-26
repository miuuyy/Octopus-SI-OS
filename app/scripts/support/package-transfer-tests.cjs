const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const { installPackages, availablePackages, main } = require("../safe-self-update.cjs");
const { runClientDoctor } = require("../../electron/clientDoctor.cjs");

module.exports = async function testTransfers(fakeLayout, tempRoot) {
  for (const scenario of ["forward", "reverse", "cycle", "duplicate", "drift", "removal", "partial", "receipt-recovery", "mode-drift", "installed-duplicate"]) {
    const bundle = fakeLayout().live, home = tempRoot();
    const app = path.join(bundle, "Contents/Resources/app");
    const library = path.join(app, "local-packages"), electron = path.join(app, "electron");
    const json = (file, value) => { fs.mkdirSync(path.dirname(file), { recursive: true }); fs.writeFileSync(file, JSON.stringify(value)); };
    fs.mkdirSync(electron, { recursive: true });
    fs.mkdirSync(path.join(home, "registry"), { recursive: true });
    fs.writeFileSync(path.join(electron, "localPackages.cjs"), `module.exports = require(${JSON.stringify(path.resolve(__dirname, "../../electron/localPackages.cjs"))});`);
    const support = path.resolve(__dirname, "../fixturePublication.cjs");
    const native = process.argv.includes("--native-publication");
    const publisher = native
      ? `require(${JSON.stringify(path.resolve(__dirname, "native-publication.cjs"))}).nativePublication()`
      : `{ ...require(${JSON.stringify(support)}).fixtureDirectoryPublisher(${JSON.stringify(home)}),
          publishFiles: require(${JSON.stringify(support)}).fixtureFilePublisher(${JSON.stringify(home)}) }`;
    fs.writeFileSync(path.join(electron, "packageStorage.cjs"), `
      const publisher = ${publisher};
      let fail = ${scenario === "receipt-recovery"}, failures = 0;
      exports.injectedFailures = () => failures;
      exports.createPackageStoragePublisher = () => ({ ...publisher, publishFiles: async (writes, condition) => {
        if (fail && writes.some(w => w.content && JSON.parse(w.content).package_version === "2.0.0")) {
          fail = false; failures++; throw new Error("Injected receipt publication failure");
        }
        return publisher.publishFiles(writes, condition);
      } });`);
    const ids = ["driver.desktop_session", "artifact.codex_context", "transfer_third"];
    const marks = ["task.summary.received", "transfer.second", "transfer.third"];
    const manifests = ids.map((id, i) => ({
      schema: "octopus.local_package", schema_version: 1, id, version: "1.0.0",
      product_kind: "application", product_id: id, release_status: "optional",
      display: { name: id, description: "Transfer fixture", developer: "Octopus", category: "sensor" },
      members: [{ registry_kind: "interface", owner: id, primary: true },
        { registry_kind: "mark", owner: marks[i] }],
    }));
    const save = () => manifests.forEach(m => json(path.join(library, "packages", m.id + ".json"), m));
    ids.forEach(id => json(path.join(library, "registry/interface", id, "interface.json"),
      { id, interface_type: "world", role: "sensor", direction: "inbound", source_names: [id] }));
    marks.forEach(id => json(path.join(library, "registry/mark", id, "mark.json"), { id, package: id, class: "success" }));
    save();
    await installPackages(bundle, home, ids);
    const receiptRoot = path.join(home, "client/package-state/receipts");
    let before = ids.map(id => fs.readFileSync(path.join(receiptRoot, id + ".json"), "utf8"));
    const moved = manifests.map(m => m.members.pop());
    manifests.forEach((m, i) => { m.version = "2.0.0"; m.members.push(moved[i]); });
    if (scenario === "cycle") manifests.forEach((m, i) => { m.members[1] = moved[(i + 1) % 3]; });
    else if (scenario === "reverse") {
      manifests[1].members.pop(); manifests[0].members.push(moved[1]);
    } else {
      manifests[0].members.pop();
      if (scenario !== "removal") manifests[1].members.push(moved[0]);
      if (scenario === "duplicate") manifests[0].members.push(moved[0]);
    }
    save();
    if (scenario === "drift") json(path.join(home, "registry/mark/task.summary.received/mark.json"),
      { id: "task.summary.received", package: "task.summary.received", class: "failure" });
    if (scenario === "mode-drift") fs.chmodSync(path.join(home, "registry/mark/task.summary.received/mark.json"), 0o700);
    if (scenario === "installed-duplicate") {
      const file = path.join(receiptRoot, ids[1] + ".json"), receipt = JSON.parse(fs.readFileSync(file));
      receipt.members.push(JSON.parse(before[0]).members.find(m => m.owner === marks[0]));
      json(file, receipt);
      before = ids.map(id => fs.readFileSync(path.join(receiptRoot, id + ".json"), "utf8"));
    }
    const args = ["--target", "app", "--build-root", path.dirname(bundle), "--home", home];
    const selected = scenario === "partial" ? ["--only=artifact.codex_context"] : [];
    if (scenario === "duplicate") {
      await assert.rejects(main(["packages", "sync", ...args]), /belongs to both/);
      assert.deepEqual(ids.map(id => fs.readFileSync(path.join(receiptRoot, id + ".json"), "utf8")), before);
      continue;
    }
    const dry = await main(["packages", "sync", "--dry-run", ...selected, ...args]);
    assert.equal(dry.applied, false);
    assert.deepEqual(ids.map(id => fs.readFileSync(path.join(receiptRoot, id + ".json"), "utf8")), before);
    const outcome = await main(["packages", "sync", ...selected, ...args]);
    assert.equal(require(path.join(electron, "packageStorage.cjs")).injectedFailures(), scenario === "receipt-recovery" ? 1 : 0);
    const success = ["forward", "reverse", "cycle", "receipt-recovery"].includes(scenario);
    assert.equal(outcome.results.length > 0 && outcome.results.every(r => r.repaired), success, JSON.stringify({ scenario, outcome }));
    if (!success) {
      assert.deepEqual(ids.map(id => fs.readFileSync(path.join(receiptRoot, id + ".json"), "utf8")), before);
      continue;
    }
    const catalog = await availablePackages(bundle, home);
    assert.deepEqual(catalog.errors, []);
    assert.ok(catalog.packages.every(p => p.installed && p.receipt_state === "valid" && !p.update_available && !p.error));
    const roots = { sourceRegistryRoot: path.join(library, "registry"), activeRegistryRoot: path.join(home, "registry"),
      libraryRoot: library, stateRoot: path.join(home, "client/package-state") };
    const doctor = await runClientDoctor({ roots });
    assert.deepEqual(doctor.findings.filter(f => f.code.startsWith("package.")), [], JSON.stringify(doctor.findings));
    const again = await main(["packages", "sync", ...args]);
    assert.deepEqual(again.results, []);
    for (const m of manifests) {
      const receipt = JSON.parse(fs.readFileSync(path.join(receiptRoot, m.id + ".json")));
      assert.deepEqual(receipt.members.map(x => x.registry_kind + "/" + x.owner).sort(),
        m.members.map(x => x.registry_kind + "/" + x.owner).sort());
    }
  }
};
