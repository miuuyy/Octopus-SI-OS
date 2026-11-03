const assert = require("node:assert/strict");
const fs = require("node:fs/promises");
const path = require("node:path");
const os = require("node:os");
const { setTimeout: delay } = require("node:timers/promises");
const { test } = require("node:test");

test("run progress refreshes exact records without invalidating configuration or model catalogues", async () => {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), "oct-projection-cycle-"));
  let catalogReads = 0, catalogFailure = false;
  const localPackages = require("../electron/localPackages.cjs");
  localPackages.readLocalPackageCatalog = async () => {
    catalogReads++;
    if (catalogFailure) throw new Error("Package configuration unavailable");
    return { packages: [{ members: [{ registry_kind: "interface", owner: "provider" }] }], packs: [], errors: ["Package catalogue diagnostic"] };
  };
  const { createProjectionStore } = require("../electron/projection.cjs");
  const changes = [];
  let home = path.join(root, "first");
  const provider = { id: "provider", interface_type: "world", role: "provider", source_names: ["model"],
    interface_driver_config: { private_config: { model: "first" } } };
  for (const name of ["first", "second", "source"]) {
    for (const directory of ["registry/interface/provider", "registry/tool", "state/ee_instances", "state/runs", "client/package-state", "ledger"]) {
      await fs.mkdir(path.join(root, name, directory), { recursive: true });
    }
    await fs.writeFile(path.join(root, name, "registry/interface/provider/interface.json"), JSON.stringify(provider));
    await fs.writeFile(path.join(root, name, "ledger/movements.jsonl"), "");
  }
  const store = createProjectionStore({ resolveRepoRoot: () => root, octopusHome: () => home,
    sourceRegistry: () => path.join(home, "registry"), localPackageRegistry: () => path.join(root, "source", "registry"),
    projectEePolicies: async () => { throw new Error("Policy projection diagnostic"); },
    onChange: (_generation, kind) => changes.push(kind) });
  const read = (method, params = {}) => store.read({ contract_version: 1, method, params });
  const changed = async (kind) => {
    const until = Date.now() + 3000;
    while (!changes.includes(kind) && Date.now() < until) await delay(20);
    assert.ok(changes.includes(kind), `Missing ${kind} change: ${changes}`);
    changes.length = 0;
  };
  try {
    assert.equal((await read("projection.operator.providers.list")).providers[0].model, "first");
    await changed("structure");
    assert.equal(catalogReads, 0, "model discovery never waits on workspace or package inspection");
    await read("projection.tools.list");
    assert.equal(catalogReads, 0, "tool descriptors do not require package inventory");
    const diagnostics = (await read("projection.runtime.snapshot")).projection.warnings;
    assert.deepEqual(diagnostics.filter(message => /diagnostic|Client Agent catalog is absent/.test(message)), [
      "Package catalogue diagnostic",
      `Client Agent catalog is absent: ${path.join(home, "client/agents/catalog.json")}`,
      "Policy projection diagnostic",
    ]);
    const initialReads = catalogReads;
    const privateRoot = path.join(home, "registry/interface/provider_private");
    await fs.mkdir(privateRoot);
    await fs.writeFile(path.join(privateRoot, "interface.json"), JSON.stringify({ ...provider, id: "provider_private", copy_of: "provider" }));
    await changed("structure");
    assert.equal((await read("projection.operator.providers.list")).providers.length, 2);
    assert.equal(catalogReads, initialReads, "private task interfaces do not change installed package owners");
    await fs.rm(privateRoot, { recursive: true });
    await changed("structure");
    assert.equal((await read("projection.operator.providers.list")).providers.length, 1);
    assert.equal(catalogReads, initialReads);
    const run = { id: "run", ee: "ee", worker: "worker", trace: "trace", status: "idle", active_checkpoints: [], checkpoints: [] };
    const runPath = path.join(home, "state/runs/run.json");
    await fs.writeFile(runPath, JSON.stringify(run));
    await changed("runs");
    assert.equal((await read("projection.runs.list", { include_ledger: false })).runs[0].record_status, "idle");
    assert.equal(catalogReads, initialReads);
    const setup = await read("projection.operator.os.workspace_setup");
    assert.equal(setup.status, "not_configured", JSON.stringify(setup));
    const receipt = path.join(home, "client-state/workspace/receipts/broken.json");
    await fs.mkdir(path.dirname(receipt), { recursive: true });
    await fs.writeFile(receipt, "invalid");
    await changed("runs");
    assert.equal((await read("projection.operator.os.workspace_setup")).status, "degraded");
    assert.equal(catalogReads, initialReads, "workspace receipts do not change model or package declarations");
    await fs.unlink(receipt); await changed("runs");
    assert.equal((await read("projection.operator.os.workspace_setup")).status, "not_configured");
    await fs.writeFile(runPath, "invalid");
    await changed("runs");
    const invalid = await read("projection.runs.list", { include_ledger: false });
    assert.equal(invalid.runs.length, 0); assert.equal(invalid.invalid_run_records.length, 1);
    await fs.writeFile(runPath, JSON.stringify({ ...run, status: "running" }));
    await changed("runs");
    assert.equal((await read("projection.runs.list", { include_ledger: false })).runs[0].record_status, "running");
    assert.equal(catalogReads, initialReads);
    await fs.writeFile(path.join(home, "client/package-state/maintenance-lock.sqlite"), "lock");
    await delay(200);
    assert.deepEqual(changes, []);
    provider.interface_driver_config.private_config.model = "second";
    await fs.writeFile(path.join(home, "registry/interface/provider/interface.json"), JSON.stringify(provider));
    await fs.writeFile(runPath, JSON.stringify(run));
    await changed("structure");
    assert.equal((await read("projection.operator.providers.list")).providers[0].model, "second");
    assert.equal(catalogReads, initialReads, "model discovery remains independent of package inspection");
    await read("projection.operator.os.local_packages");
    assert.equal(catalogReads, initialReads + 1);
    await fs.mkdir(path.join(home, "registry/interface/provider/packages"));
    await fs.writeFile(path.join(home, "registry/interface/provider/packages/tool.py"), "pass\n");
    await changed("files");
    await read("projection.operator.os.local_packages");
    assert.equal(catalogReads, initialReads + 2, "owner files must refresh package diagnostics");
    catalogFailure = true; store.invalidate();
    const beforeChatReads = catalogReads;
    assert.deepEqual((await read("projection.operator.chats.list")).chats, []);
    for (const [method, params] of [
      ["projection.operator.os.scope", {}], ["projection.operator.os.workspace_setup", {}],
      ["projection.operator.chat.messages", { run_id: "run" }], ["projection.operator.providers.status", {}],
      ["projection.operator.run.activity", { run_id: "run" }], ["projection.tools.list", {}],
      ["projection.worker_environments.list", {}],
      ["projection.runtime.topology", { include_ledger: false, focused_run_ids: ["run"] }],
      ["projection.runtime.topology", { include_ledger: true, focused_run_ids: ["run"] }],
    ]) await read(method, params);
    await assert.rejects(read("projection.operator.chat.search", { query: "text", task_run_ids: ["run"] }), /unavailable run: run/);
    assert.equal(catalogReads, beforeChatReads, "task and descriptor reads do not audit unrelated package files");
    await assert.rejects(read("projection.operator.os.local_packages"), /configuration unavailable/);
    assert.equal((await read("projection.operator.providers.list")).providers[0].model, "second");
    assert.equal((await read("projection.movements.list", { limit: 1 })).next_seq, 0);
    catalogFailure = false;
    assert.equal((await read("projection.operator.providers.list")).providers[0].model, "second");
    await changed("structure");
    await read("projection.operator.os.local_packages");
    const beforeStateReads = catalogReads;
    await fs.mkdir(path.join(home, "interfaces/provider"), { recursive: true });
    await fs.writeFile(path.join(home, "interfaces/provider/stream-state.json"), "{}");
    await changed("ledger");
    assert.equal((await read("projection.runs.list", { include_ledger: false })).runs.length, 1);
    assert.equal(catalogReads, beforeStateReads);
    await fs.writeFile(path.join(home, "interfaces/provider/environment.json"), JSON.stringify({ schema_version: 1, interface: "provider", values: {} }));
    await changed("structure");
    assert.equal((await read("projection.operator.providers.list")).providers[0].model, "second");
    assert.equal(catalogReads, beforeStateReads, "private runtime state does not change installed package files");
    const { readConnectedApplicationIdsByRun } = require("../electron/projection/applicationPackages.cjs");
    const sourceRoot = path.join(root, "source/registry");
    const applicationRoot = path.join(sourceRoot, "interface/source_app");
    const manifest = { schema_version: 3, id: "source_app", name: "Source", description: "Source application",
      category: "plugin", developer: "Test", version: "1", logo: "logo.svg", interfaces: ["source_app"],
      artifact_bindings: [{ artifact_id: "context" }], task_capabilities: { tools: [] } };
    await fs.mkdir(applicationRoot);
    for (const [name, value] of [["interface.json", { id: "source_app" }], ["application.json", manifest]]) {
      await fs.writeFile(path.join(applicationRoot, name), JSON.stringify(value));
    }
    await fs.writeFile(path.join(applicationRoot, "logo.svg"), '<svg xmlns="http://www.w3.org/2000/svg"/>');
    const capability = { tool: "tool", interface: "door", worker_environment_kind: "room" };
    const connectedProjection = { localPackageRegistryRoot: sourceRoot,
      descriptors: { interface: [{ application_manifest: { ...manifest, id: "active_app", interfaces: ["active_app"], task_capabilities: { tools: [capability] } } }] },
      runRecords: ["complete", "missing_capability", "missing_artifact", "inactive", "outside"].map(id => ({
        run_id: id, ee_id: id === "outside" ? "outside" : "ee",
        checkpoints: [{ status: id === "inactive" ? "made" : "projected", interface: "active_app",
          out_interfaces: ["source_app"], artifact_binding_ids: id === "missing_artifact" ? [] : ["context"] }],
      })) };
    const connectionScope = { environments: [{ ee_id: "ee" }], client_runs: [
      { run_id: "complete", authoring_profile: { selected_tools: [capability] } },
    ] };
    assert.deepEqual(Object.fromEntries(await readConnectedApplicationIdsByRun(connectedProjection, connectionScope)), {
      complete: ["active_app", "source_app"], missing_capability: ["source_app"],
    });
    await fs.symlink(path.join(applicationRoot, "interface.json"), path.join(applicationRoot, "unsafe-link"));
    assert.deepEqual(Object.fromEntries(await readConnectedApplicationIdsByRun(connectedProjection, connectionScope)), {
      complete: ["active_app"],
    }, "source-only connections retain full package validation");
    home = path.join(root, "second");
    assert.equal((await read("projection.operator.providers.list")).providers[0].model, "first");
    assert.deepEqual((await read("projection.runs.list", { include_ledger: false })).runs, []);
  } finally { store.close(); await fs.rm(root, { recursive: true, force: true }); }
});

test("EE policy projection rebuilds when policy changes while its build is in flight", async () => {
  const builds = [];
  const store = require("../electron/projection.cjs").createProjectionContext({ resolveRepoRoot: () => os.tmpdir(), octopusHome: () => os.tmpdir(),
    projectEePolicies: () => new Promise((resolve) => builds.push(resolve)) });
  const first = store.cachedEePolicyProjection();
  await delay(0); assert.equal(builds.length, 1);
  store.invalidateConfigurationProjection(false, true);
  builds[0]({ environments: [], issues: ["stale policy"] });
  await delay(0); assert.equal(builds.length, 2);
  builds[1]({ environments: [], issues: ["current policy"] });
  assert.deepEqual((await first).issues, ["current policy"]);
  assert.deepEqual((await store.cachedEePolicyProjection()).issues, ["current policy"]);
  assert.equal(builds.length, 2);
  store.invalidateConfigurationProjection(true, false);
  assert.deepEqual((await store.cachedEePolicyProjection()).issues, ["current policy"]);
  assert.equal(builds.length, 2);
});

test("workspace inspection reads while a writer holds its lock and leaves pending recovery untouched", async () => {
  const root = await fs.realpath(await fs.mkdtemp(path.join(os.tmpdir(), "oct-workspace-read-")));
  const state = path.join(root, "client-state/workspace");
  for (const relative of ["registry/interface", "state/runs", "state/ee_instances", "client-state/workspace/transactions/.stage-operation123456-ab12ab12ab12ab12"]) {
    await fs.mkdir(path.join(root, relative), { recursive: true });
  }
  const staged = path.join(state, "transactions/.stage-operation123456-ab12ab12ab12ab12");
  await fs.writeFile(path.join(staged, "pending.json"), "{}");
  let release, acquired;
  const ready = new Promise(resolve => { acquired = resolve; });
  const held = require("../electron/clientFileLock.cjs").withClientFileLock({ directory: state,
    filename: "transaction-lock.sqlite", waitMs: 30000, busyCode: "busy", busyMessage: "busy" }, async () => {
    acquired(); await new Promise(resolve => { release = resolve; });
  });
  try {
    await ready;
    const result = await Promise.race([
      require("../electron/workspaceSetup.cjs").inspectWorkspaceSetupState({ octopusHomeRoot: root, registryRoot: path.join(root, "registry") }),
      delay(2000).then(() => assert.fail("a read waited on the writer")),
    ]);
    assert.equal(result.status, "not_configured", JSON.stringify(result));
    assert.equal(await fs.readFile(path.join(staged, "pending.json"), "utf8"), "{}");
    await assert.rejects(fs.stat(path.join(state, "receipts")), { code: "ENOENT" });
  } finally { release?.(); await held; await fs.rm(root, { recursive: true, force: true }); }
});

test("run movements do not starve a snapshot and the next read sees the newer records", async () => {
  const { invalidationProjection } = require("../electron/projection/invalidation.cjs");
  const root = await fs.mkdtemp(path.join(os.tmpdir(), "oct-snapshot-read-"));
  let finish, count = 0;
  const store = { octopusHome: () => root, resolveSourceRegistry: () => "/source",
    resolveLocalPackageRegistry: () => "/packages", resolveLocalPackageLibrary: () => "/library",
    invalidateConfigurationProjection() {}, onChange() {},
    buildFileProjection: () => { count++; return new Promise(resolve => { finish = resolve; }); },
  };
  const methods = invalidationProjection(store);
  try {
    const pending = methods.cachedFileProjection("/repo", false);
    methods.invalidateProjection(false, "runs");
    finish({ runRecords: ["observed"] });
    assert.deepEqual((await pending).runRecords, ["observed"]);
    const next = methods.cachedFileProjection("/repo", false);
    assert.equal(count, 2);
    finish({ runRecords: ["observed", "new"] });
    assert.deepEqual((await next).runRecords, ["observed", "new"]);
  } finally { methods.closeProjectionWatchers(); await fs.rm(root, { recursive: true, force: true }); }
});

test("topology without ledger preserves structure without reading malformed history", async () => {
  const { createProjectionContext } = require("../electron/projection.cjs");
  const root = await fs.mkdtemp(path.join(os.tmpdir(), "oct-topology-no-ledger-"));
  await fs.mkdir(path.join(root, "ledger"));
  await fs.writeFile(path.join(root, "ledger/movements.jsonl"), `${JSON.stringify({ seq: 0, lamport: 0,
    recorded_at_unix_ms: 1000, trace: "trace", kind: "InterfaceOutbound",
    from: { Interface: "input" }, to: { Interface: "output" } })}\nmalformed ledger row\n`);
  try {
    for (const params of [{ include_ledger: false }, { include_ledger: true }, {}]) {
      const store = createProjectionContext({ resolveRepoRoot: () => root, octopusHome: () => root });
      let ledgerReads = 0;
      const readLedger = store.readLedgerProjection;
      store.readLedgerProjection = (...args) => { ledgerReads++; return readLedger(...args); };
      store.buildFileProjection = async () => ({
        repoRoot: root, octopusHome: root, registryRoot: path.join(root, "registry"),
        stateRoot: path.join(root, "state"), runRecordsRoot: path.join(root, "state/runs"),
        durableRoot: path.join(root, "ledger"), generation: store.projectionGeneration,
        generatedAt: "2026-01-01T00:00:00Z", descriptors: {}, eeInstances: [],
        workerEnvironments: [{ worker_environment_id: "room", run_id: "run", status: "failed", live_available: false,
          opened_at_movement_seq: 0, last_activity_movement_seq: 0 }],
        runRecords: [{ run_id: "run", ee_id: "ee", worker: "worker", status: "running",
          record_status: "running", active_checkpoint_count: 1,
          checkpoints: [{ checkpoint_id: "cp", interface: "input", out_interfaces: ["output"],
            required_verified_artifacts: [{ mark: "done", artifact_label: "result" }] }] }],
        invalidRunRecords: [], warnings: [], ledgerRows: [], invalidLedgerRows: [], ledgerRevision: "",
        structureMemo: new Map(), ledgerMemo: new Map(),
      });
      store.withFileProjectionPackages = async (projection) => projection;
      try {
        const result = await store.readProjection({ contract_version: 1, method: "projection.runtime.topology", params });
        const includeLedger = params.include_ledger !== false;
        assert.equal(ledgerReads, includeLedger ? 1 : 0);
        assert.equal(result.runsRaw.runs[0].run_id, "run");
        assert.equal(result.runsRaw.runs[0].record_status, "running");
        assert.equal(result.runCheckpointsRaw.run_checkpoints[0].checkpoints[0].checkpoint_id, "cp");
        assert.equal(result.runCheckpointsRaw.run_checkpoints[0].checkpoints[0].required_verified_artifacts[0].outcome, "declared");
        assert.equal(result.runDetailsRaw.run.detail.worker_environment.status, "failed");
        assert.equal(result.workerEnvironmentsRaw.worker_environments[0].live_available, false);
        assert.deepEqual(result.bindingsRaw.bindings.map(row => row.interface_id), ["input", "output"]);
        assert.equal(result.snapshotRaw.projection.ledger_loaded, includeLedger);
        assert.equal(result.snapshotRaw.projection.invalid_ledger_rows.length, includeLedger ? 1 : 0);
        assert.equal(result.snapshotRaw.projection.warnings.length, includeLedger ? 1 : 0);
        assert.equal(result.movementsRaw.movements.length, includeLedger ? 1 : 0);
        if (!includeLedger) {
          assert.equal(result.workerEnvironmentsRaw.worker_environments[0].opened_at_unix_ms, undefined);
          await store.readProjection({ contract_version: 1, method: "projection.runtime.topology", params: { include_ledger: true } });
          const readsBefore = ledgerReads;
          const warm = await store.readProjection({ contract_version: 1, method: "projection.runtime.topology", params });
          assert.equal(ledgerReads, readsBefore);
          assert.equal(warm.snapshotRaw.projection.ledger_loaded, true);
          assert.equal(warm.snapshotRaw.projection.invalid_ledger_rows.length, 1);
          assert.equal(warm.workerEnvironmentsRaw.worker_environments[0].opened_at_unix_ms, 1000);
          assert.equal(warm.workerEnvironmentsRaw.worker_environments[0].last_activity_at_unix_ms, 1000);
          assert.deepEqual(warm.runsRaw, result.runsRaw);
          assert.deepEqual(warm.runCheckpointsRaw, result.runCheckpointsRaw);
          assert.deepEqual(warm.movementsRaw, result.movementsRaw);
        }
      } finally { store.close(); }
    }
  } finally { await fs.rm(root, { recursive: true, force: true }); }
});
