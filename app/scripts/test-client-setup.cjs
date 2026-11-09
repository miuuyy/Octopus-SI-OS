const assert = require("node:assert/strict");
const crypto = require("node:crypto");
const fs = require("node:fs/promises");
const os = require("node:os");
const path = require("node:path");

const { createClientSetupStore } = require("../electron/clientSetup.cjs");

const STEPS = ["packages", "extensions", "workspace", "provider"];
const KINDS = {
  packages: "package_install_evidence",
  extensions: "extension_lifecycle_receipt",
  provider: "provider_observation",
  workspace: "workspace_receipt",
};

async function main() {
  const temporary = await fs.realpath(await fs.mkdtemp(path.join(os.tmpdir(), "octopus-client-setup-")));
  try {
    if (process.argv[2] === "--plan-admission") return await rendererPlanAdmission(path.join(temporary, "renderer"));
    if (process.argv[2] === "--live-progress") return await readWhileApplying(path.join(temporary, "live-progress"));
    await relaunchSelection(path.join(temporary, "launch"));
    await rendererPlanAdmission(path.join(temporary, "renderer"));
    await coldPresetLoader(path.join(temporary, "cold-preset"));
    await normalLifecycle(path.join(temporary, "normal"));
    await readWhileApplying(path.join(temporary, "live-progress"));
    await explicitSignInBoundary(path.join(temporary, "sign-in"));
    await checkpointEphemeralOwnerCandidate(path.join(temporary, "owner-candidate"));
    await currentOperationRecovery(path.join(temporary, "current"));
    await refuseSecondUnfinishedOperation(path.join(temporary, "singleton"));
    await refuseCredentialStoreOnlyPlan(path.join(temporary, "provider-proof"));
    await recordProbeFailure(path.join(temporary, "probe-failure"));
    for (const step of STEPS) await resumeAfterCommittedFailure(path.join(temporary, `failure-${step}`), step);
    await refuseSealAfterProofDrift(path.join(temporary, "proof-drift"));
    await refuseCorruptOperation(path.join(temporary, "corrupt"));
  } finally {
    await fs.rm(temporary, { recursive: true, force: true });
  }
}

async function relaunchSelection(root) {
  const { resolveClientLaunchSelection: read, saveClientLaunchSelection: save } = require("../electron/clientLaunchSelection.cjs");
  const selectionPath = path.join(root, "launch.json"), userRoot = path.join(root, "user");
  const resolve = env => read({ selectionPath, userRoot, env });
  const chosen = resolve({ OCTOPUS_HOME: path.join(root, "Fresh"), CODEX_HOME: path.join(root, "Fresh", "codex") });
  save(selectionPath, chosen);
  const restarted = JSON.parse(require("node:child_process").execFileSync(process.execPath, ["-e",
    `console.log(JSON.stringify(require(process.argv[1]).resolveClientLaunchSelection({selectionPath:process.argv[2],userRoot:process.argv[3],env:{}})))`,
    path.resolve(__dirname, "../electron/clientLaunchSelection.cjs"), selectionPath, userRoot], { encoding: "utf8" }));
  assert.deepEqual(restarted, chosen);
  assert.equal(resolve({ OCTOPUS_HOME: path.join(root, "Other") }).codexHome, undefined);
  assert.equal(resolve({ CODEX_HOME: path.join(root, "auth") }).home, chosen.home);
  assert.throws(() => resolve({ OCTOPUS_HOME: "relative" }), /absolute/);
  await fs.writeFile(selectionPath, "{}");
  assert.throws(() => resolve({}), /malformed/);
  await fs.unlink(selectionPath);
  await fs.symlink(path.join(root, "missing"), selectionPath);
  assert.throws(() => resolve({}));
}

async function rendererPlanAdmission(root) {
  const ts = require("typescript");
  const app = path.resolve(__dirname, "..");
  const source = async (file) => fs.readFile(path.join(app, file), "utf8");
  const extract = (text, name, scope) => {
    const tree = ts.createSourceFile("source.tsx", text, ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
    const node = tree.statements.find((value) => ts.isFunctionDeclaration(value) && value.name?.text === name);
    assert.ok(node, name);
    const code = ts.transpileModule("return (" + node.getText(tree).replace(/^export\s+/, "") + ");", { compilerOptions: { target: ts.ScriptTarget.ES2022 } }).outputText;
    return Function(...Object.keys(scope), code)(...Object.values(scope));
  };
  const module = { exports: {} };
  const code = ts.transpileModule(await source("src/onboarding/workspaceSetup.ts"), { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 } }).outputText;
  const storage = new Map();
  const window = { localStorage: { getItem: key => storage.get(key) ?? null, setItem: (key, value) => storage.set(key, value), removeItem: key => storage.delete(key) } };
  Function("module", "exports", "crypto", "window", code)(module, module.exports, crypto.webcrypto, window);
  const renderer = module.exports;
  const completedDraft = renderer.saveWorkspaceSetupDraft({ ...renderer.createWorkspaceSetupDraft("custom"), step: "applying" });
  for (const [key, value] of storage) storage.set(key, JSON.stringify({ ...JSON.parse(value), updatedAt: "2026-11-10T00:00:00.000Z" }));
  assert.equal(renderer.clearCompletedWorkspaceSetupDraft(completedDraft), null, "saving unchanged choices again cannot fail completed setup");
  assert.equal(storage.size, 0);
  assert.equal(renderer.clearCompletedWorkspaceSetupDraft(completedDraft), null, "another view may have already retired the completed draft");
  const newerDraft = renderer.saveWorkspaceSetupDraft({ ...completedDraft, spaceName: "New choices" });
  assert.deepEqual(renderer.clearCompletedWorkspaceSetupDraft(completedDraft), newerDraft, "completion preserves choices edited in another view");
  assert.deepEqual(renderer.readWorkspaceSetupDraft(), newerDraft);
  const { readLocalPackageCatalog } = require("../electron/localPackages.cjs");
  const catalog = await readLocalPackageCatalog({ sourceRegistryRoot: path.join(app, "../registry"),
    libraryRoot: path.join(app, "distribution/local-packages"), activeRegistryRoot: path.join(root, "registry"), stateRoot: path.join(root, "package-state") });
  assert.deepEqual(catalog.errors, []);
  const declaredPack = catalog.packs.find((entry) => entry.id === "codex");
  assert.ok(declaredPack);
  // This fixture checks renderer/main plan agreement; executable import closure is tested by test-local-package-closure.cjs.
  const native = { ...declaredPack, components: declaredPack.components.map((entry) => ({
    ...entry, source_digest: digest(`renderer-plan-fixture:${entry.package_id}`),
  })) };
  const components = native.components.map((entry) => ({
    packageId: entry.package_id, sourceDigest: entry.source_digest, productKind: entry.product_kind, productId: entry.product_id,
  }));
  const pack = { id: native.id, manifestDigest: native.manifest_digest, components,
    recipe: { ...native.workspace_setup.home, task: native.workspace_setup.task,
      project_defaults: native.workspace_setup.project_defaults, agent_presets: native.workspace_setup.agent_presets } };
  const main = await source("electron/main.cjs");
  const stableClientSetupJson = extract(main, "stableClientSetupJson", {});
  const admit = extract(main, "requireClientSetupPack", { createHash: crypto.createHash, stableClientSetupJson });
  const compile = extract(await source("src/components/onboarding/WorkspaceSetupGate.tsx"), "compileClientSetupPlanWithDigest", {
    compileWorkspaceInitializeRequest: renderer.compileWorkspaceInitializeRequest, DIGEST: /^[a-f0-9]{64}$/, ProductSetupError: Error,
  });
  const draft = renderer.createWorkspaceSetupDraft("custom");
  Object.assign(draft, { providerModel: "gpt-6-astra", reasoningEffort: "high", serviceTier: "ultrafast" });
  const selection = { packageIds: components.map((entry) => entry.packageId),
    selectedRecommended: native.components.filter((entry) => entry.selection === "recommended").map((entry) => entry.package_id),
    extensions: (native.extensions ?? []).map((entry) => ({ extensionId: entry.extension_id, version: "1.0.0", sourceDigest: "sha256:" + "3".repeat(64), desiredState: "available" })),
    projectKeysByPackage: Object.fromEntries(components.map((entry) => [entry.packageId, draft.projects.map((project) => project.key)])) };
  const request = renderer.compileWorkspaceInitializeRequest(draft, pack.recipe, pack);
  assert.deepEqual(request.home.doorHostSlots, { system_workspace_env: { workspace_root: "/", host_filesystem_root: "/" } });
  assert.ok(request.home.selectedWorkerEnvironmentKinds.includes("filesystem_workspace"));
  const cli = require("../cli/setup.cjs").compileSetupPlan(native, { operationId: draft.operationId, workspaceName: "Fresh", workspaceGoal: "Fresh",
    projects: [], model: draft.providerModel, reasoningEffort: draft.reasoningEffort, recommended: [] });
  assert.deepEqual(cli.workspace.request.home.doorHostSlots, request.home.doorHostSlots);
  assert.throws(() => renderer.compileWorkspaceInitializeRequest(draft, { ...pack.recipe, door_host_slots: { system_workspace_env: { host_filesystem_root: "relative" } } }, pack), /absolute path/);
  const requestDigest = await renderer.workspaceInitializeRequestDigest(request);
  const plan = compile(draft, pack, selection, requestDigest);
  assert.equal(admit({ packs: [native] }, plan), native);
  for (const components of [
    pack.components.slice(1),
    [...pack.components, pack.components[0]],
    pack.components.map((entry, index) => index === 0 ? { ...entry, sourceDigest: "" } : entry),
  ]) assert.throws(() => compile(draft, { ...pack, components }, selection, requestDigest), /one verified package snapshot/);
  for (const mutate of [
    (request) => { request.home.doorHostSlots = {}; },
    (request) => { request.home.doorHostSlots.system_workspace_env.host_filesystem_root = "/limited"; },
    (request) => { delete request.task; },
    (request) => { request.task.workerInterfaceId = request.home.workerInterfaceId; },
    (request) => { request.task.contextArtifactBindings = []; },
    (request) => { request.task.selectedWorkerEnvironmentKinds = []; },
  ]) {
    const invalid = structuredClone(plan);
    mutate(invalid.workspace.request);
    assert.throws(() => admit({ packs: [native] }, invalid), /workspace recipe/);
    if (invalid.workspace.request.task) {
      invalid.workspace.request_digest = await renderer.workspaceInitializeRequestDigest(invalid.workspace.request);
      assert.throws(() => admit({ packs: [native] }, invalid), /workspace recipe/);
    }
  }
}

async function coldPresetLoader(root) {
  root = path.join(await fs.realpath(path.dirname(root)), path.basename(root));
  const { installLocalPackages, readLocalPackageCatalog, withInstalledAgentPreset } = require("../electron/localPackages.cjs");
  const { createOperatorChatClientLoader } = require("../electron/operatorChatClientContract.cjs");
  const vm = require("node:vm");
  const home = path.join(root, "home");
  const roots = { publication: require("./support/native-publication.cjs").nativePublication(), sourceRegistryRoot: path.join(root, "source"), activeRegistryRoot: path.join(home, "registry"), libraryRoot: path.join(root, "library"), stateRoot: path.join(home, "client", "package-state") };
  const write = async (file, value) => { await fs.mkdir(path.dirname(file), { recursive: true }); await fs.writeFile(file, `${JSON.stringify(value, null, 2)}\n`); };
  const worker = "system_worker_fixture";
  const provider = "world_provider_fixture";
  for (const id of ["world_operator_chat", worker, provider]) await write(path.join(roots.sourceRegistryRoot, "interface", id, "interface.json"), { id, interface_type: id === worker ? "system" : "world", role: id === worker ? "worker" : "provider" });
  await write(path.join(roots.sourceRegistryRoot, "interface", worker, "policy.json"), { id: worker, role_artifacts: [], ceiling: { tools: [], wek: [], mark_outcomes: [], interfaces: [provider] } });
  await fs.writeFile(path.join(roots.sourceRegistryRoot, "interface", "world_operator_chat", "client.cjs"), "exports.readOperatorChatArtifacts = async () => ({artifacts: []}); exports.submitOperatorTurn = async () => ({}); exports.planOperatorChatSurface = async (_options, request) => ({providerModel: request.providerModel, authoringProfile: {ee_id: request.eeId, worker_choice_interface: request.workerInterfaceId, door_host_slots: {}}});\n");
  const config = { schema_version: 2, interface: "world_operator_chat", run: { ee: "ee_default", worker, context_artifact_bindings: [], run_prefix: "run_" }, executables: { topology_planner: "packages/bin/topology" } };
  await write(path.join(roots.sourceRegistryRoot, "interface", "world_operator_chat", "client.json"), config);
  const life = { processes: [{ bin: "owner://packages/bin/client", authority: { sandbox_mode: "enforced", allow_network: false } }] };
  await write(path.join(roots.sourceRegistryRoot, "interface", "world_operator_chat", "life.json"), life);
  await write(path.join(roots.libraryRoot, "packages", "world_operator_chat.json"), {
    schema: "octopus.local_package", schema_version: 1, id: "world_operator_chat", version: "1.0.0", product_kind: "application", product_id: "world_operator_chat", release_status: "optional",
    display: { name: "Planner", description: "Cold planner package fixture.", developer: "Octopus", category: "provider" },
    members: ["world_operator_chat", worker, provider].map((owner) => ({ registry_kind: "interface", owner, ...(owner === "world_operator_chat" ? { primary: true } : {}) })),
  });
  const recipe = { id: "fixture", component_package_id: "world_operator_chat", name: "Fixture", icon: "goggles", worker_interface_id: worker, provider_interface_id: provider, context_artifact_bindings: [], selected_worker_environment_kinds: [], selected_application_ids: [] };
  await write(path.join(roots.libraryRoot, "packs", "fixture.json"), {
    schema: "octopus.local_application_pack", schema_version: 1, id: "fixture", version: "1.0.0", name: "Fixture", description: "Cold planner fixture.",
    components: [{ package_id: "world_operator_chat", selection: "included", reason: "Planner owner." }],
    workspace_setup: { selection: "included", home: { provider_interface_id: provider, worker_interface_id: worker, context_artifact_bindings: [], selected_worker_environment_kinds: [] },
      task: { worker_interface_id: worker, context_artifact_bindings: [], selected_worker_environment_kinds: [] }, project_defaults: [], agent_presets: [recipe] },
  });
  const catalog = await readLocalPackageCatalog(roots);
  assert.deepEqual(catalog.errors, []);
  await installLocalPackages({ ...roots, operationId: "cold_preset_install", packageIds: ["world_operator_chat"], expectedSourceDigests: { world_operator_chat: catalog.packages[0].source_digest } });
  const configPath = path.join(roots.activeRegistryRoot, "interface", "world_operator_chat", "client.json");
  const configured = { ...config, run: { ...config.run, ee: "ee_existence", worker: "system_worker_experiment", context_artifact_bindings: [{ id: "recipient-instructions" }] } };
  const loader = createOperatorChatClientLoader({ packageRoots: () => roots, clientPath: () => path.join(roots.activeRegistryRoot, "interface", "world_operator_chat", "client.cjs") });
  assert.equal(loader.loadedProof(), null);
  const mainSource = await fs.readFile(path.join(__dirname, "../electron/main.cjs"), "utf8");
  const functions = mainSource.slice(mainSource.indexOf("async function planWorkspaceHomeSurface("), mainSource.indexOf("async function planWorkspaceAgentPreset("));
  const invoke = vm.runInNewContext(`${functions}; withWorkspaceAgentPreset`, {
    path, createHash: crypto.createHash, resolveOctopusHome: () => home, resolveInstallationRoot: () => root,
    loadOperatorChatClient: loader.load, withInstalledAgentPreset, clientSetupPackageRoots: () => roots,
    requiredString: (value) => { assert.equal(typeof value, "string"); assert.ok(value.trim()); return value; },
  });
  let timeout;
  try {
    const result = await Promise.race([
      invoke({ home, packId: "fixture", presetId: "fixture", expectedPackManifestDigest: catalog.packs[0].manifest_digest, operationId: "cold_preset_create", projectEeId: "ee_project", providerInterfaceId: provider, providerModel: "selected-model", reasoningEffort: "high" }, path.join(home, "state", "ee_instances"), async (preset) => { await preset.verify(); return preset.settings; }),
      new Promise((_, reject) => { timeout = setTimeout(() => reject(new Error("Cold planner deadlocked on the package lock")), 2000); }),
    ]);
    assert.equal(result.provider_interface_id, provider);
    assert.equal(result.reasoning_effort, "high");
    assert.ok(loader.loadedProof());
  } finally { clearTimeout(timeout); }
  const fresh = () => createOperatorChatClientLoader({ packageRoots: () => roots, clientPath: () => path.join(roots.activeRegistryRoot, "interface", "world_operator_chat", "client.cjs") });
  await write(configPath, configured);
  const lifePath = path.join(roots.activeRegistryRoot, "interface", "world_operator_chat", "life.json");
  const configuredLife = { processes: [{ ...life.processes[0], authority: { ...life.processes[0].authority, sandbox_mode: "disabled" } }] };
  await write(lifePath, configuredLife);
  const configuredLoader = fresh();
  await configuredLoader.load();
  assert.equal(configuredLoader.loadedProof().configuration.schemaVersion, 2);
  for (const authored of [
    { ...configured, executables: { topology_planner: "other/bin" } },
    { ...configured, run: { ...configured.run, run_prefix: "other_" } },
  ]) {
    await write(configPath, authored);
    await fresh().load();
  }
  for (const invalid of [
    { ...configured, schema_version: 99 },
    { ...configured, interface: "world_other" },
    { ...configured, run: { ...configured.run, context_artifact_bindings: [{ id: "same" }, { id: "same" }] } },
  ]) {
    await write(configPath, invalid);
    await assert.rejects(fresh().load(), (error) => error.code === "operator_chat_client_contract_mismatch");
  }
  await write(configPath, configured);
  await write(lifePath, { processes: [{ ...configuredLife.processes[0], bin: "owner://other" }] });
  await fresh().load();
  await write(lifePath, { processes: [{ bin: "" }] });
  await assert.rejects(fresh().load(), /valid declared processes/);
  await write(lifePath, configuredLife);
  const clientFile = path.join(roots.activeRegistryRoot, "interface", "world_operator_chat", "client.cjs");
  await fs.appendFile(clientFile, "\nexports.locallyAuthored = true;\n");
  delete require.cache[clientFile];
  assert.equal((await fresh().load()).locallyAuthored, true);
  await fs.writeFile(clientFile, "module.exports = {};\n");
  delete require.cache[clientFile];
  await assert.rejects(fresh().load(), /does not export readOperatorChatArtifacts/);

}

async function checkpointEphemeralOwnerCandidate(root) {
  const ownerState = path.join(root, "owner-state.json");
  const ownerSet = owners(ownerState);
  let applyCount = 0;
  let candidateProbeCount = 0;
  let persistedProbeCount = 0;
  ownerSet.provider = {
    probe: async (_plan, context) => {
      if (context.candidateEvidence?.accountReadable === true) {
        candidateProbeCount += 1;
        return [proofReference("provider")];
      }
      if (context.previousProof?.references?.[0]?.id === "provider.proof") {
        persistedProbeCount += 1;
        return [proofReference("provider")];
      }
      return null;
    },
    apply: async () => {
      applyCount += 1;
      return { accountReadable: true };
    },
  };
  const operationId = "workspace_owner_candidate_001";
  const store = createClientSetupStore({ root: path.join(root, "setup-state"), owners: ownerSet });
  await store.begin({ operationId, plan: setupPlan(operationId) });
  const progress = [];
  const completed = await store.resume(operationId, { onProgress: (value) => progress.push(value) });
  assert.equal(completed.status, "complete");
  assert.ok(progress.slice(0, -1).every((value) => value.percent < 100));
  assert.deepEqual(progress.slice(-5, -1).map((value) => [value.step, value.phase]), [["packages", "verifying"], ["extensions", "verifying"], ["workspace", "verifying"], ["provider", "verifying"]]);
  assert.equal(progress.at(-1).phase, "sealed");
  assert.equal(progress.at(-1).percent, 100);
  assert.equal(applyCount, 1);
  assert.equal(candidateProbeCount, 1);
  assert.equal(persistedProbeCount, 1);
}

async function normalLifecycle(root) {
  const ownerState = path.join(root, "owner-state.json");
  const store = createClientSetupStore({ root: path.join(root, "setup-state"), owners: owners(ownerState) });
  const operationId = "workspace_setup_normal_001";
  const plan = setupPlan(operationId);
  const begun = await store.begin({ operationId, plan });
  assert.equal(begun.status, "in_progress");
  assert.ok(begun.steps.every((step) => step.state === "pending"));
  assert.equal((await store.readCurrent()).operationId, operationId);
  const completed = await store.resume(operationId);
  assert.equal(completed.status, "complete");
  assert.match(completed.receiptDigest, /^[a-f0-9]{64}$/);
  assert.ok(completed.steps.every((step) => step.state === "complete"));
  assert.equal((await store.readCurrent()).receiptDigest, completed.receiptDigest);
  const receipt = JSON.parse(await fs.readFile(
    path.join(root, "setup-state", "receipts", `${operationId}.json`),
    "utf8",
  ));
  assert.equal(receipt.proof_semantics, "sequential_owner_observations");
  assert.ok(receipt.proofs.every((proof) => proof.observed_at && proof.references.every((reference) => reference.evidence)));
  const repeated = await store.resume(operationId);
  assert.equal(repeated.receiptDigest, completed.receiptDigest);
  await fs.writeFile(
    path.join(root, "setup-state", "operations", `${operationId}.json`),
    "{corrupt completed journal\n",
    "utf8",
  );
  assert.equal((await store.readCurrent()).receiptDigest, completed.receiptDigest);
  await assert.rejects(store.begin({
    operationId,
    plan: { ...plan, provider: { interface_id: "world_provider_other", required_state: "account_readable" } },
  }), /another immutable plan/);
}

async function readWhileApplying(root) {
  const ownerState = path.join(root, "owners.json");
  const operationId = "workspace_live_progress_001";
  let release, entered;
  const blocked = new Promise(resolve => { release = resolve; });
  const applying = new Promise(resolve => { entered = resolve; });
  const ownerSet = owners(ownerState);
  const apply = ownerSet.extensions.apply;
  ownerSet.extensions.apply = async (...args) => { entered(); await blocked; return apply(...args); };
  const options = { root: path.join(root, "setup-state"), owners: ownerSet };
  const store = createClientSetupStore(options);
  await store.begin({ operationId, plan: setupPlan(operationId) });
  const originalProgress = [], reopenedProgress = [];
  const running = store.resume(operationId, { onProgress: event => originalProgress.push(event) });
  await applying;
  const reopened = createClientSetupStore(options);
  const repeated = reopened.resume(operationId, { onProgress: event => reopenedProgress.push(event) });
  let timer;
  try {
    const reads = await Promise.race([
      Promise.all([reopened.read(operationId), reopened.readCurrent()]),
      new Promise((_, reject) => { timer = setTimeout(() => reject(new Error("Setup status waits for the installation to finish")), 1000); }),
    ]);
    for (const state of reads) {
      assert.equal(state.status, "in_progress");
      assert.equal(state.operationId, operationId);
      assert.deepEqual(state.steps.map(step => step.state), ["complete", "pending", "pending", "pending"]);
    }
    assert.deepEqual(reopenedProgress, [originalProgress.at(-1)], "a returning window receives the current installation phase");
  } finally {
    clearTimeout(timer);
    release();
    await Promise.all([running, repeated]);
  }
  assert.equal((await reopened.readCurrent()).status, "complete");
  assert.equal(reopenedProgress.at(-1).phase, "sealed", "the returning window receives completion");
  assert.deepEqual(await readOwnerState(ownerState), Object.fromEntries(STEPS.map(step => [step, 1])));
}

async function explicitSignInBoundary(root) {
  const operationId = "workspace_sign_in_001";
  const ownerSet = owners(path.join(root, "owners.json"));
  const provider = ownerSet.provider.apply;
  let signIns = 0;
  ownerSet.provider.apply = async (...args) => { signIns++; return provider(...args); };
  const options = { root: path.join(root, "setup-state"), owners: ownerSet };
  const store = createClientSetupStore(options);
  await store.begin({ operationId, plan: setupPlan(operationId) });
  const prepared = await store.resume({ operationId, stopBefore: "provider" });
  assert.equal(prepared.status, "in_progress");
  assert.equal(prepared.steps.find(step => step.step === "provider").state, "pending");
  assert.ok(prepared.steps.filter(step => step.step !== "provider").every(step => step.state === "complete"));
  assert.equal(signIns, 0);
  const resumed = createClientSetupStore(options);
  await resumed.resume({ operationId, stopBefore: "provider" });
  assert.equal(signIns, 0);
  await assert.rejects(resumed.resume({ operationId, stopBefore: "unknown" }), /Unsupported.*stop point/);
  const completed = await resumed.resume({ operationId });
  assert.equal(completed.status, "complete");
  assert.equal(signIns, 1);
  assert.equal((await resumed.resume({ operationId })).receiptDigest, completed.receiptDigest);
  assert.equal(signIns, 1);
}

async function currentOperationRecovery(root) {
  const ownerState = path.join(root, "owner-state.json");
  const setupRoot = path.join(root, "setup-state");
  const operationId = "workspace_current_recovery_001";
  const firstStore = createClientSetupStore({ root: setupRoot, owners: owners(ownerState) });
  await firstStore.begin({ operationId, plan: setupPlan(operationId) });
  await fs.unlink(path.join(setupRoot, "current.json"));
  const restarted = createClientSetupStore({ root: setupRoot, owners: owners(ownerState) });
  const current = await restarted.readCurrent();
  assert.equal(current.status, "in_progress");
  assert.equal(current.operationId, operationId);
  assert.equal(JSON.parse(await fs.readFile(path.join(setupRoot, "current.json"), "utf8")).operation_id, operationId);
}

async function refuseSecondUnfinishedOperation(root) {
  const ownerState = path.join(root, "owner-state.json");
  const store = createClientSetupStore({ root: path.join(root, "setup-state"), owners: owners(ownerState) });
  const first = "workspace_singleton_first_001";
  const second = "workspace_singleton_second_002";
  await store.begin({ operationId: first, plan: setupPlan(first) });
  await assert.rejects(
    store.begin({ operationId: second, plan: setupPlan(second) }),
    /still in progress/,
  );
}

async function refuseCredentialStoreOnlyPlan(root) {
  const ownerState = path.join(root, "owner-state.json");
  const store = createClientSetupStore({ root: path.join(root, "setup-state"), owners: owners(ownerState) });
  const operationId = "workspace_provider_proof_001";
  const plan = setupPlan(operationId);
  plan.provider.required_state = "configured";
  await assert.rejects(
    store.begin({ operationId, plan }),
    /required_state must be equal to constant/,
  );
}

async function recordProbeFailure(root) {
  const ownerState = path.join(root, "owner-state.json");
  const ownerSet = owners(ownerState);
  ownerSet.packages.probe = async () => {
    throw new Error("simulated owner proof read failure");
  };
  const store = createClientSetupStore({ root: path.join(root, "setup-state"), owners: ownerSet });
  const operationId = "workspace_probe_failure_001";
  await store.begin({ operationId, plan: setupPlan(operationId) });
  await assert.rejects(store.resume(operationId), /owner proof read failure/);
  const failed = await store.read(operationId);
  assert.equal(failed.lastError.step, "packages");
  assert.match(failed.lastError.message, /owner proof read failure/);
}

async function resumeAfterCommittedFailure(root, failedStep) {
  const ownerState = path.join(root, "owner-state.json");
  const operationId = `workspace_failure_${failedStep}_001`;
  const plan = setupPlan(operationId);
  let injected = false;
  const firstStore = createClientSetupStore({
    root: path.join(root, "setup-state"),
    owners: owners(ownerState, {
      [failedStep]: async () => {
        if (injected) return;
        injected = true;
        throw new Error(`simulated process loss after ${failedStep} owner commit`);
      },
    }),
  });
  await firstStore.begin({ operationId, plan });
  await assert.rejects(firstStore.resume(operationId), new RegExp(`after ${failedStep} owner commit`));
  const interrupted = await firstStore.read(operationId);
  assert.equal(interrupted.status, "in_progress");
  assert.equal(interrupted.lastError.step, failedStep);

  const resumedStore = createClientSetupStore({ root: path.join(root, "setup-state"), owners: owners(ownerState) });
  const completed = await resumedStore.resume(operationId);
  assert.equal(completed.status, "complete");
  const persisted = await readOwnerState(ownerState);
  assert.ok(STEPS.every((step) => persisted[step] === 1), JSON.stringify(persisted));
}

async function refuseSealAfterProofDrift(root) {
  const ownerState = path.join(root, "owner-state.json");
  const operationId = "workspace_proof_drift_001";
  const base = owners(ownerState);
  let workspaceProbes = 0;
  base.workspace.probe = async () => {
    workspaceProbes += 1;
    const state = await readOwnerState(ownerState);
    if (!state.workspace || workspaceProbes > 2) return null;
    return [proofReference("workspace")];
  };
  const store = createClientSetupStore({ root: path.join(root, "setup-state"), owners: base });
  await store.begin({ operationId, plan: setupPlan(operationId) });
  await assert.rejects(store.resume(operationId), /evidence is no longer valid/);
  assert.equal((await store.read(operationId)).status, "in_progress");
  await assert.rejects(fs.readFile(path.join(root, "setup-state", "receipts", `${operationId}.json`)), /ENOENT/);
  base.workspace.probe = async () => [proofReference("workspace")];
  base.provider.probe = async () => null;
  base.provider.apply = async () => { throw new Error("Sign-in started without an explicit action"); };
  const paused = await store.resume({ operationId, stopBefore: "provider" });
  assert.equal(paused.status, "in_progress");
}

async function refuseCorruptOperation(root) {
  const ownerState = path.join(root, "owner-state.json");
  const operationId = "workspace_corrupt_operation_001";
  const store = createClientSetupStore({ root: path.join(root, "setup-state"), owners: owners(ownerState) });
  await store.begin({ operationId, plan: setupPlan(operationId) });
  await fs.writeFile(path.join(root, "setup-state", "operations", `${operationId}.json`), "{not-json\n", "utf8");
  await assert.rejects(store.read(operationId), /malformed JSON/);
}

function owners(statePath, afterCommit = {}) {
  return Object.fromEntries(STEPS.map((step) => [step, {
    probe: async () => {
      const state = await readOwnerState(statePath);
      return state[step] ? [proofReference(step)] : null;
    },
    apply: async () => {
      const state = await readOwnerState(statePath);
      state[step] = (state[step] ?? 0) + 1;
      await fs.mkdir(path.dirname(statePath), { recursive: true });
      await fs.writeFile(statePath, `${JSON.stringify(state, null, 2)}\n`, "utf8");
      await afterCommit[step]?.();
    },
  }]));
}

function proofReference(step) {
  const evidence = { owner: step, state: "ready" };
  return {
    kind: KINDS[step],
    id: `${step}.proof`,
    digest: digest(stableJson(evidence)),
    evidence,
  };
}

async function readOwnerState(file) {
  try {
    return JSON.parse(await fs.readFile(file, "utf8"));
  } catch (error) {
    if (error?.code === "ENOENT") return {};
    throw error;
  }
}

function setupPlan(operationId) {
  return {
    pack: {
      id: "codex",
      manifest_digest: digest("codex-pack"),
      selected_recommended: [],
    },
    packages: [{ id: "world_provider_codex", source_digest: digest("provider-package"), desired_state: "installed" }],
    extensions: [{
      id: "builtin.provider-credential-repair",
      version: "1.0.0",
      source_digest: `sha256:${digest("repair-extension")}`,
      desired_state: "installed",
    }],
    provider: { interface_id: "world_provider_codex", required_state: "account_readable" },
    workspace: {
      request_digest: digest("workspace-request"),
      request: { action: "initialize_workspace", operationId },
      project_defaults: [],
    },
  };
}

function digest(value) {
  return crypto.createHash("sha256").update(value).digest("hex");
}

function stableJson(value) {
  if (Array.isArray(value)) return `[${value.map(stableJson).join(",")}]`;
  if (value && typeof value === "object") {
    return `{${Object.keys(value).sort().map((key) => `${JSON.stringify(key)}:${stableJson(value[key])}`).join(",")}}`;
  }
  return JSON.stringify(value);
}

void main().catch((error) => {
  process.stderr.write(`${error?.stack || error}\n`);
  process.exitCode = 1;
});
