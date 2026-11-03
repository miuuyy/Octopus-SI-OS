const assert = require("node:assert/strict");
const fs = require("node:fs/promises");
const os = require("node:os");
const path = require("node:path");
const { spawnSync } = require("node:child_process");
const {
  applyLocalApplicationPack,
  installLocalPackages,
  readLocalPackageCatalog,
  readLocalPackageSourcePackage,
} = require("../electron/localPackages.cjs");
const { createProjectionStore } = require("../electron/projection.cjs");
const {
  planOperatorChatSurface,
} = require("../../registry/interface/world_operator_chat/client.cjs");

const repoRoot = path.resolve(__dirname, "../..");
const sourceRegistryRoot = path.join(repoRoot, "registry");
const libraryRoot = path.join(repoRoot, "app", "distribution", "local-packages");

async function writeJson(file, value) {
  await fs.mkdir(path.dirname(file), { recursive: true });
  await fs.writeFile(file, `${JSON.stringify(value, null, 2)}\n`, "utf8");
}

async function assertPackMembers() {
  const operator = JSON.parse(await fs.readFile(path.join(libraryRoot, "packages", "world_operator_chat.json"), "utf8"));
  assert.deepEqual(operator.members.map(member => `${member.registry_kind}/${member.owner}`).sort(), [
    "artifact/artifact-role-memory-observer",
    "ee_template/client_maintenance",
    "interface/system_client_maintenance",
    "interface/system_context_memory_projection",
    "interface/system_interface_authoring",
    "interface/system_worker_client_maintenance",
    "interface/system_worker_memory_observer",
    "interface/system_worker_operator_home",
    "interface/system_worker_operator_task",
    "interface/world_operator_chat",
    "mark/memory.capture.completed",
    "mark/memory.experience.admitted",
    "mark/tool.call.allowed",
    "mark/tool.result.corresponds",
    "verifier/memory_capture_outcome",
    "verifier/memory_experience",
    "verifier/operator_approval",
    "verifier/operator_input",
    "verifier/tool_call_allowed",
    "verifier/tool_result_correspondence",
  ]);
  const pack = JSON.parse(await fs.readFile(path.join(libraryRoot, "packs", "codex.json"), "utf8"));
  assert.equal(pack.workspace_setup.home.provider_interface_id, "world_provider_codex");
  assert.equal(pack.workspace_setup.project_defaults[0].package_id, "world_provider_codex");
}

async function copyBaseline(activeRegistryRoot) {
  for (const template of ["existence", "operator_space", "project"]) {
    await fs.cp(
      path.join(sourceRegistryRoot, "ee_template", template),
      path.join(activeRegistryRoot, "ee_template", template),
      { recursive: true, errorOnExist: true, force: false },
    );
  }
}

async function assertInterfaceImportClosure(publicationExecutable) {
  const root = await fs.realpath(await fs.mkdtemp(path.join(os.tmpdir(), "octopus-interface-import-closure-")));
  try {
    const source = path.join(root, "source");
    const active = path.join(root, "active");
    const library = path.join(root, "library");
    const state = path.join(root, "state");
    const importer = path.join(source, "interface", "world_importer");
    const owner = path.join(source, "interface", "world_code_owner");
    const executable = path.join(owner, "packages", "bin", "life");
    const manifestFile = path.join(library, "packages", "application.importer.json");
    const manifest = (includeOwner) => ({
      schema: "octopus.local_package",
      schema_version: 1,
      id: "application.importer",
      version: "1.0.0",
      product_kind: "application",
      product_id: "world_importer",
      release_status: "optional",
      display: { name: "Importer", description: "Explicit interface import closure fixture.", developer: "Octopus", category: "provider" },
      members: [
        { registry_kind: "interface", owner: "world_importer", primary: true },
        ...(includeOwner ? [{ registry_kind: "interface", owner: "world_code_owner" }] : []),
      ],
    });
    const roots = { publication: require("./support/native-publication.cjs").nativePublication(publicationExecutable), sourceRegistryRoot: source, activeRegistryRoot: active, libraryRoot: library, stateRoot: state };
    await writeJson(path.join(importer, "interface.json"), { id: "world_importer" });
    await writeJson(path.join(importer, "life.json"), { processes: [{ bin: "interface://world_code_owner/packages/bin/life" }] });
    await writeJson(path.join(owner, "interface.json"), { id: "world_code_owner" });
    await fs.mkdir(path.dirname(executable), { recursive: true });
    await fs.writeFile(executable, "owner executable\n", "utf8");
    await writeJson(manifestFile, manifest(false));
    await assert.rejects(
      installLocalPackages({ ...roots, operationId: "closure_install_001", packageIds: ["application.importer"], expectedSourceDigests: { "application.importer": "0".repeat(64) } }),
      /manifest members do not include 'interface\/world_code_owner'/,
    );
    await assert.rejects(fs.stat(path.join(active, "interface", "world_importer")), { code: "ENOENT" });
    await writeJson(manifestFile, manifest(true));
    await writeJson(path.join(owner, "interface.json"), { id: "world_wrong_owner" });
    await assert.rejects(
      readLocalPackageSourcePackage({ sourceRegistryRoot: source, libraryRoot: library, packageId: "application.importer" }),
      /drifted descriptor id/,
    );
    await writeJson(path.join(owner, "interface.json"), { id: "world_code_owner" });
    await fs.rm(executable);
    await assert.rejects(
      readLocalPackageSourcePackage({ sourceRegistryRoot: source, libraryRoot: library, packageId: "application.importer" }),
      /does not exist in the 'world_code_owner' snapshot/,
    );
    await fs.writeFile(executable, "owner executable\n", "utf8");
    const closed = await readLocalPackageSourcePackage({ sourceRegistryRoot: source, libraryRoot: library, packageId: "application.importer" });
    assert.deepEqual(closed.members.map((member) => member.owner).sort(), ["world_code_owner", "world_importer"]);
    await writeJson(manifestFile, { ...manifest(false), required_packages: ["application.code_owner"] });
    await writeJson(path.join(library, "packages", "application.code_owner.json"), {
      ...manifest(false), id: "application.code_owner", product_id: "world_code_owner",
      members: [{ registry_kind: "interface", owner: "world_code_owner", primary: true }],
    });
    const external = await readLocalPackageSourcePackage({ sourceRegistryRoot: source, libraryRoot: library, packageId: "application.importer" });
    assert.deepEqual(external.members.map(member => member.owner), ["world_importer"]);
    const dependency = await readLocalPackageSourcePackage({ sourceRegistryRoot: source, libraryRoot: library, packageId: "application.code_owner" });
    const peer = path.join(source, "interface", "world_peer");
    const peerManifest = path.join(library, "packages", "application.peer.json");
    await writeJson(path.join(peer, "interface.json"), { id: "world_peer" });
    await writeJson(path.join(peer, "life.json"), { processes: [{ bin: "interface://world_code_owner/packages/bin/life" }] });
    await writeJson(peerManifest, { ...manifest(false), id: "application.peer", product_id: "world_peer",
      required_packages: ["application.code_owner"], members: [{ registry_kind: "interface", owner: "world_peer", primary: true }] });
    const scans = new Map(), reads = new Map(), readdir = fs.readdir, readFile = fs.readFile;
    fs.readdir = async function (file, ...args) { scans.set(file, (scans.get(file) ?? 0) + 1); return readdir.call(this, file, ...args); };
    fs.readFile = async function (file, ...args) { reads.set(file, (reads.get(file) ?? 0) + 1); return readFile.call(this, file, ...args); };
    let catalog;
    try { catalog = await readLocalPackageCatalog(roots); }
    finally { fs.readdir = readdir; fs.readFile = readFile; }
    assert.equal(catalog.packages.length, 3);
    assert.ok(catalog.packages.every(entry => entry.available && !entry.installed && entry.receipt_state === "missing"));
    assert.equal(scans.get(path.join(library, "packages")), 1, "one catalog read loads its library once");
    assert.equal(scans.get(owner), 1, "shared interface dependencies have one source snapshot per catalog read");
    assert.equal(reads.get(manifestFile), 1, "dependency validation reuses the catalog's parsed library");
    const dependentRecords = value => value.packages.filter(entry => ["application.importer", "application.peer"].includes(entry.id));
    await writeJson(path.join(owner, "interface.json"), { id: "world_wrong_owner" });
    assert.ok(dependentRecords(await readLocalPackageCatalog(roots)).every(entry => !entry.available && /drifted descriptor id/.test(entry.error)));
    await writeJson(path.join(owner, "interface.json"), { id: "world_code_owner" });
    await fs.rm(executable);
    assert.ok(dependentRecords(await readLocalPackageCatalog(roots)).every(entry => !entry.available && /does not exist/.test(entry.error)));
    await fs.symlink(path.join(owner, "interface.json"), executable);
    assert.ok((await readLocalPackageCatalog(roots)).packages.every(entry => !entry.available && /symbolic link/.test(entry.error)));
    await fs.rm(executable);
    await fs.writeFile(executable, "owner executable\n");
    assert.deepEqual(await readLocalPackageCatalog(roots), catalog, "a later read must recover after invalid source files are repaired");
    const dependencyManifest = path.join(library, "packages", "application.code_owner.json");
    const originalManifest = await fs.readFile(dependencyManifest, "utf8");
    await writeJson(dependencyManifest, { ...JSON.parse(originalManifest), version: "1.0.1" });
    assert.equal((await readLocalPackageCatalog(roots)).packages.find(entry => entry.id === "application.code_owner").version, "1.0.1");
    await fs.writeFile(dependencyManifest, originalManifest);
    await fs.rm(peerManifest);
    await fs.rm(peer, { recursive: true });
    await installLocalPackages({ ...roots, operationId: "closure_install_002",
      packageIds: ["application.importer", "application.code_owner"], expectedSourceDigests: {
        "application.importer": external.source_digest, "application.code_owner": dependency.source_digest,
      } });
    assert.equal(await fs.readFile(path.join(active, "interface", "world_code_owner", "packages", "bin", "life"), "utf8"), "owner executable\n");
    await fs.rm(executable);
    await assert.rejects(readLocalPackageSourcePackage({ sourceRegistryRoot: source, libraryRoot: library, packageId: "application.importer" }), /does not exist/);
    await fs.writeFile(executable, "owner executable\n");
    await writeJson(manifestFile, manifest(false));
    await assert.rejects(readLocalPackageSourcePackage({ sourceRegistryRoot: source, libraryRoot: library, packageId: "application.importer" }), /no explicit required package/);
  } finally {
    await fs.rm(root, { recursive: true, force: true });
  }
}

function buildTopologyBinary() {
  const result = spawnSync("cargo", [
    "build",
    "-p",
    "octopus-operator-chat-client",
    "--bin",
    "octopus-operator-chat-topology",
  ], {
    cwd: repoRoot,
    encoding: "utf8",
  });
  if (result.status !== 0) {
    throw new Error(result.stderr || result.stdout || "operator chat topology build failed");
  }
}

async function assertPackageCopyBudget(packageIds, executable, root) {
  let bytes = (await fs.stat(executable)).size;
  for (const packageId of packageIds) {
    const source = await readLocalPackageSourcePackage({ sourceRegistryRoot, libraryRoot, packageId });
    bytes += source.members.reduce((total, member) => total + member.files.reduce((size, file) => size + file.size, 0), 0);
  }
  const copyBudget = 512 * 1024 * 1024;
  assert.ok(bytes + 8 * 1024 * 1024 < copyBudget, `Fixture exceeds copy budget: ${bytes}`);
  const space = await fs.statfs(root);
  assert.ok(space.bavail * space.bsize > copyBudget + 256 * 1024 * 1024, "Fixture requires its copy budget plus 256 MiB free reserve");
  process.stdout.write(`fixture declared copies: ${bytes} bytes plus <8MiB metadata\n`);
}

async function run(preparedExecutable) {
  if (!preparedExecutable) buildTopologyBinary();
  const executable = preparedExecutable ?? path.join(repoRoot, "target", "debug", "octopus-operator-chat-topology");
  await assertInterfaceImportClosure(executable);
  const root = await fs.realpath(await fs.mkdtemp(path.join(os.tmpdir(), "octopus-codex-pack-closure-")));
  try {
    const binary = path.join(root, "target", "debug", "octopus-operator-chat-topology");
    await fs.mkdir(path.dirname(binary), { recursive: true });
    await fs.copyFile(executable, binary);
    await fs.chmod(binary, 0o700);
    const octopusHomeRoot = path.join(root, "octopus-home");
    const activeRegistryRoot = path.join(octopusHomeRoot, "registry");
    const stateRoot = path.join(octopusHomeRoot, "client", "package-state");
    const eeInstancesRoot = path.join(octopusHomeRoot, "state", "ee_instances");
    const runRecordsRoot = path.join(octopusHomeRoot, "state", "runs");
    await Promise.all([
      copyBaseline(activeRegistryRoot),
      fs.mkdir(eeInstancesRoot, { recursive: true }),
      fs.mkdir(runRecordsRoot, { recursive: true }),
      fs.mkdir(path.join(octopusHomeRoot, "ledger"), { recursive: true }),
    ]);
    await fs.writeFile(path.join(octopusHomeRoot, "ledger", "movements.jsonl"), "", "utf8");
    await writeJson(path.join(octopusHomeRoot, "host.json"), {
      host_manifest_version: 1,
      root_refs: {
        run_records_root: runRecordsRoot,
        ee_instances_root: eeInstancesRoot,
      },
    });
    await writeJson(path.join(eeInstancesRoot, "ee_existence.json"), {
      ee_id: "ee_existence",
      template_name: "existence",
      parent_ee_id: null,
      goal: "existence",
      config: {},
    });
    await writeJson(path.join(eeInstancesRoot, "ee_operator_space.json"), {
      ee_id: "ee_operator_space",
      template_name: "operator_space",
      parent_ee_id: "ee_existence",
      goal: "Operate this space through Home.",
      config: {},
    });

    const roots = {
      publication: require("./support/native-publication.cjs").nativePublication(executable),
      sourceRegistryRoot,
      activeRegistryRoot,
      libraryRoot,
      stateRoot,
    };
    let catalog = await readLocalPackageCatalog(roots);
    assert.deepEqual(catalog.errors, []);
    const pack = catalog.packs.find((entry) => entry.id === "codex");
    assert.ok(pack);
    assert.equal(pack.workspace_setup.selection, "included");
    assert.deepEqual(pack.workspace_setup.home.selected_worker_environment_kinds, ["browser_session", "code_mode", "desktop_session", "filesystem_workspace", "system_room"]);
    assert.deepEqual(pack.workspace_setup.project_defaults, [
      { package_id: "world_provider_codex", default_for_new_tasks: true },
      { package_id: "driver.browser_session", default_for_new_tasks: true },
      { package_id: "driver.desktop_session", default_for_new_tasks: true },
      { package_id: "driver.filesystem_workspace", default_for_new_tasks: true },
      { package_id: "driver.process_session", default_for_new_tasks: false },
      { package_id: "driver.git_worktree", default_for_new_tasks: false },
    ]);
    const included = pack.components
      .filter((component) => component.selection === "included");
    const selected = [...new Set(included.flatMap((component) =>
      catalog.packages.find((entry) => entry.id === component.package_id).install_packages.map((entry) => entry.id)))];
    await assertPackageCopyBudget(selected, executable, root);
    const expectedSourceDigests = Object.fromEntries(included.map((component) => [
      component.package_id,
      component.source_digest,
    ]));
    const result = await applyLocalApplicationPack({
      ...roots,
      operationId: "codex_closure_install_001",
      packId: "codex",
      selectedRecommended: [],
      expectedSourceDigests,
    });
    assert.equal(result.changed, true);

    for (const owner of await fs.readdir(path.join(activeRegistryRoot, "interface"))) {
      if (owner === ".octopus-publication") continue;
      const descriptor = JSON.parse(await fs.readFile(path.join(activeRegistryRoot, "interface", owner, "interface.json"), "utf8"));
      const continuity = descriptor.interface_driver_config?.private_config?.operator_continuity;
      for (const contract of Object.values(continuity ?? {}).filter(value => value && typeof value === "object")) {
        for (const requirement of Object.values(contract).filter(value => value && typeof value === "object" && value.verifier)) {
          const verifierRoot = path.join(activeRegistryRoot, "verifier", requirement.verifier);
          const verifier = JSON.parse(await fs.readFile(path.join(verifierRoot, "verifier.json"), "utf8"));
          assert.equal(verifier.id, requirement.verifier);
          assert.ok((await fs.stat(path.join(verifierRoot, verifier.script))).isFile());
          const mark = JSON.parse(await fs.readFile(path.join(activeRegistryRoot, "mark", requirement.mark, "mark.json"), "utf8"));
          assert.equal(mark.id, requirement.mark);
        }
      }
    }

    catalog = await readLocalPackageCatalog(roots);
    const installedPack = catalog.packs.find((entry) => entry.id === "codex");
    assert.equal(installedPack.required_components_materialized, true);
    assert.ok(included.every((component) => (
      installedPack.components.find((entry) => entry.package_id === component.package_id)?.receipt_state === "valid"
    )));

    const projectionStore = createProjectionStore({
      resolveRepoRoot: () => repoRoot,
      sourceRegistry: () => sourceRegistryRoot,
      localPackageRegistry: () => sourceRegistryRoot,
      localPackageLibrary: () => libraryRoot,
      octopusHome: () => octopusHomeRoot,
      projectEePolicies: async () => ({ environments: [], issues: [] }),
    });
    try {
      const applications = await projectionStore.read({
        contract_version: 1,
        method: "projection.operator.os.applications",
      });
      const operatorHome = applications.applications.find((entry) => entry.id === "world_operator_chat");
      assert.equal(operatorHome?.installed, true);
      assert.equal(operatorHome?.valid, true);
      assert.equal(operatorHome?.source_kind, "local");
    } finally {
      projectionStore.close();
    }

    const home = installedPack.workspace_setup.home;
    const task = installedPack.workspace_setup.task;
    const plan = await planOperatorChatSurface({
      octopusHome: () => octopusHomeRoot,
      installationRoot: () => root,
      registryRoot: () => activeRegistryRoot,
      eeInstancesRoot: () => eeInstancesRoot,
    }, {
      chatId: "freshhome0000000000000001",
      cycle: "freshcycle00000000000001",
      providerInterfaceId: home.provider_interface_id,
      providerModel: "gpt-5.6-sol",
      workerInterfaceId: home.worker_interface_id,
      eeId: "ee_existence",
      clientSurfaceRole: "home",
      doorHostSlots: home.door_host_slots,
      contextArtifactBindings: home.context_artifact_bindings,
      selectedWorkerEnvironmentKinds: home.selected_worker_environment_kinds,
    });
    assert.equal(plan.modelInterfaceId.startsWith(`${home.provider_interface_id}_`), true);
    assert.equal(plan.workerInterfaceId.startsWith(`${home.worker_interface_id}_`), true);
    assert.equal(plan.authoringProfile.ee_id, "ee_existence");
    assert.equal(plan.authoringProfile.worker_choice_interface, home.worker_interface_id);
    assert.deepEqual(plan.authoringProfile.selected_worker_environment_kinds, home.selected_worker_environment_kinds);
    assert.deepEqual(plan.authoringProfile.door_host_slots, home.door_host_slots);
    assert.ok(plan.requiredToolIds.some((toolId) => toolId.startsWith("file.write__authoring_")));
    assert.ok(plan.writes.some((write) => write.tag.startsWith("run:")));
    const main = JSON.parse(plan.writes.find((write) => write.tag === `run:${plan.runId}`).content);
    const voice = main.interface_state[plan.chatInterfaceId].voice_context;
    assert.notEqual(voice.run_id, main.id);
    const call = JSON.parse(plan.writes.find((write) => write.tag === `run:${voice.run_id}`).content);
    assert.equal(call.ee, main.ee);
    assert.equal(call.interface_state[voice.control_interface_id].home_run, main.id);
    assert.ok(call.interface_state[voice.provider_id].run_authoring_binding.writer_run);
    assert.equal(main.interface_state[voice.provider_id], undefined);
    await writeJson(path.join(eeInstancesRoot, "ee_project_preflight.json"), {
      ee_id: "ee_project_preflight",
      template_name: "project",
      parent_ee_id: "ee_existence",
      goal: "Prove task authoring in a newly created Project.",
      config: {},
    });
    const linkedResultRequirement = {
      mark: "task.summary.received",
      artifactLabel: "task.summary",
      verifier: "artifact_exists",
      params: {},
    };
    const taskPlan = await planOperatorChatSurface({
      octopusHome: () => octopusHomeRoot,
      installationRoot: () => root,
      registryRoot: () => activeRegistryRoot,
      eeInstancesRoot: () => eeInstancesRoot,
    }, {
      chatId: "freshproject00000000000001",
      cycle: "freshprojectcycle00000001",
      providerInterfaceId: home.provider_interface_id,
      providerModel: "gpt-5.6-sol",
      workerInterfaceId: task.worker_interface_id,
      eeId: "ee_project_preflight",
      clientSurfaceRole: "task",
      doorHostSlots: {},
      externalInputs: [{
        checkpointId: "cp_link_result_probe",
        interfaceId: "system_worker_operator_home",
        artifactNames: ["task.summary"],
        requiredVerifiedArtifacts: [linkedResultRequirement],
      }],
      externalRelays: [{
        checkpointId: "cp_link_relay_probe",
        interfaceId: "system_interface_authoring",
        outboundInterfaceIds: ["system_room"],
        requiredVerifiedArtifacts: [linkedResultRequirement],
      }],
      contextArtifactBindings: task.context_artifact_bindings,
      selectedWorkerEnvironmentKinds: task.selected_worker_environment_kinds,
    });
    assert.equal(taskPlan.clientSurfaceRole, "task");
    assert.equal(taskPlan.authoringProfile.ee_id, "ee_project_preflight");
    assert.equal(taskPlan.authoringProfile.worker_choice_interface, task.worker_interface_id);
    assert.ok(taskPlan.authoringProfile.context_artifact_bindings.some((binding) => binding.id === "artifact-worker-role-operator-task"));
    assert.ok(!taskPlan.authoringProfile.context_artifact_bindings.some((binding) => binding.id === "artifact-worker-role-operator-home"));
    assert.deepEqual(taskPlan.authoringProfile.external_inputs[0].artifact_names, ["task.summary"]);
    assert.deepEqual(
      taskPlan.authoringProfile.external_inputs[0].required_verified_artifacts,
      [{
        mark: "task.summary.received",
        artifact_label: "task.summary",
        verifier: "artifact_exists",
        params: {},
      }],
    );
    assert.deepEqual(
      taskPlan.authoringProfile.external_relays[0].required_verified_artifacts,
      taskPlan.authoringProfile.external_inputs[0].required_verified_artifacts,
    );
    for (const owner of [
      ["interface", "world_operator_chat"],
      ["interface", "system_interface_authoring"],
      ["interface", "system_worker_operator_home"],
      ["interface", "system_room"],
      ["interface", "world_provider_codex"],
      ["worker_environment_kind", "system_room"],
      ["tool", "file.write"],
      ["mark", "tool.call.allowed"],
      ["verifier", "tool_call_allowed"],
      ["artifact", "artifact-worker-role-template"],
      ["artifact", "artifact-grounding-system-architecture"],
    ]) {
      assert.equal((await fs.stat(path.join(activeRegistryRoot, ...owner))).isDirectory(), true);
    }
  } finally {
    await fs.rm(root, { recursive: true, force: true });
  }
}

async function boundedAgentPresetFixture(executable) {
  const crypto = require("node:crypto");
  const vm = require("node:vm");
  const childProcess = require("node:child_process");
  const { withInstalledAgentPreset } = require("../electron/localPackages.cjs");
  const { createOperatorChatClientLoader } = require("../electron/operatorChatClientContract.cjs");
  const { planClientAgentTemplate, readClientAgentCatalog } = require("../electron/clientAgentCatalog.cjs");
  const { initializeWorkspace, commitClientAuthoringTransaction } = require("../electron/workspaceSetup.cjs");
  const { mutateOctopusOsWorkspace } = require("../electron/projection.cjs");
  const root = await fs.realpath(await fs.mkdtemp("/private/tmp/octopus-cu-preset-"));
  const home = path.join(root, "home");
  const active = path.join(home, "registry");
  const roots = { publication: require("./support/native-publication.cjs").nativePublication(executable), sourceRegistryRoot, activeRegistryRoot: active, libraryRoot, stateRoot: path.join(home, "client", "package-state") };
  const originalSpawn = childProcess.spawn;
  try {
    const catalog = await readLocalPackageCatalog(roots);
    assert.deepEqual(catalog.errors, []);
    const pack = catalog.packs.find((entry) => entry.id === "codex");
    const recipe = pack.workspace_setup.agent_presets.find((entry) => entry.id === "computer-use");
    const selected = [...new Set([recipe.component_package_id, "world_operator_chat", recipe.provider_interface_id].flatMap((id) =>
      catalog.packages.find((entry) => entry.id === id).install_packages.map((entry) => entry.id)))];
    await assertPackageCopyBudget(selected, executable, root);
    const binary = path.join(root, "target", "debug", "octopus-operator-chat-topology");
    await fs.mkdir(path.dirname(binary), { recursive: true });
    await fs.copyFile(executable, binary);
    await fs.chmod(binary, 0o700);
    await installLocalPackages({ ...roots, operationId: "cu_preset_install_001", packageIds: selected, expectedSourceDigests: Object.fromEntries(selected.map((id) => [id, catalog.packages.find((entry) => entry.id === id).source_digest])) });
    await copyBaseline(active);
    const providerId = recipe.provider_interface_id;
    const providerFile = path.join(active, "interface", providerId, "interface.json");
    const providerBytes = await fs.readFile(providerFile);
    const provider = JSON.parse(providerBytes);
    const model = provider.interface_driver_config.private_config.model;
    const effort = provider.interface_driver_config.private_config.reasoning_effort;
    const eeRoot = path.join(home, "state", "ee_instances");
    await fs.mkdir(path.join(home, "state", "runs"), { recursive: true });
    await fs.mkdir(path.join(home, "ledger"), { recursive: true });
    await fs.writeFile(path.join(home, "ledger", "movements.jsonl"), "");
    await writeJson(path.join(eeRoot, "ee_existence.json"), { ee_id: "ee_existence", template_name: "existence", parent_ee_id: null, goal: "existence", config: {} });
    await writeJson(path.join(home, "host.json"), { host_manifest_version: 1, root_refs: { ee_instances_root: eeRoot, run_records_root: path.join(home, "state", "runs") } });
    let plannerCalls = 0;
    childProcess.spawn = (command, args, options) => {
      assert.equal(command, binary);
      plannerCalls += 1;
      const child = originalSpawn(command, args, { ...options, timeout: 30000, env: { PATH: process.env.PATH, OCTOPUS_HOME: home, TMPDIR: root } });
      let outputBytes = 0;
      for (const stream of [child.stdout, child.stderr]) stream.on("data", (chunk) => { outputBytes += chunk.length; if (outputBytes > 16 * 1024 * 1024) child.kill("SIGKILL"); });
      return child;
    };
    const loader = createOperatorChatClientLoader({ packageRoots: () => roots, clientPath: () => path.join(active, "interface", "world_operator_chat", "client.cjs") });
    const mainSource = await fs.readFile(path.join(__dirname, "../electron/main.cjs"), "utf8");
    const functions = mainSource.slice(mainSource.indexOf("async function planWorkspaceHomeSurface("), mainSource.indexOf("function getCodexUsageReader("));
    const planners = vm.runInNewContext(`${functions}; ({planWorkspaceHomeSurface, planWorkspaceAgentPreset, withWorkspaceAgentPreset})`, {
      path, createHash: crypto.createHash, planClientAgentTemplate, withInstalledAgentPreset,
      resolveOctopusHome: () => home, resolveInstallationRoot: () => root, loadOperatorChatClient: loader.load, clientSetupPackageRoots: () => roots,
      requiredString: (value) => { assert.equal(typeof value, "string"); assert.ok(value.trim()); return value; },
    });
    const choice = { packId: pack.id, expectedPackManifestDigest: pack.manifest_digest, presetId: recipe.id, projectKeys: ["desktop"], providerInterfaceId: providerId, providerModel: model, reasoningEffort: effort };
    const homeRecipe = pack.workspace_setup.home;
    const taskRecipe = pack.workspace_setup.task;
    const request = {
      action: "initialize_workspace", operationId: "cu_preset_workspace_001",
      root: { key: "root", templateName: "existence", name: "Desktop work", goal: "Operate desktop tasks" },
      home: { name: "Home", providerInterfaceId: providerId, providerModel: model, reasoningEffort: effort, workerInterfaceId: homeRecipe.worker_interface_id, doorHostSlots: homeRecipe.door_host_slots, contextArtifactBindings: homeRecipe.context_artifact_bindings, selectedWorkerEnvironmentKinds: homeRecipe.selected_worker_environment_kinds },
      task: { workerInterfaceId: taskRecipe.worker_interface_id, contextArtifactBindings: taskRecipe.context_artifact_bindings, selectedWorkerEnvironmentKinds: taskRecipe.selected_worker_environment_kinds },
      projects: [{ key: "desktop", parentKey: "root", templateName: "project", name: "Desktop", goal: "Desktop tasks", presentation: { icon: "folder", color: "blue" } }],
      agentPresets: [choice],
    };
    const browserSetup = require(path.join(active, "worker_environment_kind", "browser_session", "packages", "octopus-browser-session-env", "native", "setup-host.cjs"));
    const desktopSetup = require(path.join(active, "worker_environment_kind", "desktop_session", "packages", "octopus-system-gui-env", "native", "setup-preferences.cjs"));
    let nativeHomePlan;
    const workspace = await initializeWorkspace({ publication: roots.publication, octopusHomeRoot: home, registryRoot: active, request, planOperatorSurface: async input => {
      const plan = await planners.planWorkspaceHomeSurface(input);
      if (input.request.clientSurfaceRole === "home") nativeHomePlan = plan;
      return plan;
    }, planAgentPreset: planners.planWorkspaceAgentPreset, planBrowserNativeHost: browserSetup.planBrowserNativeHost, planDesktopApplicationPreferences: desktopSetup.planDesktopApplicationPreferences });
    assert.equal(workspace.rootEeId, "ee_existence");
    const actualRoot = JSON.parse(await fs.readFile(path.join(eeRoot, "ee_existence.json")));
    assert.equal(actualRoot.parent_ee_id, null);
    assert.equal(actualRoot.goal, request.root.goal);
    const setupFile = path.join(__dirname, "../electron/workspaceSetup.cjs");
    const check = Function("require", "module", `${await fs.readFile(setupFile, "utf8")}\nreturn {validatePlannedHomeRuns, validatePlannedHomeOwnership, readMutableWorkspaceSidecar};`)(require("node:module").createRequire(setupFile), { exports: {} });
    const validationRoots = { registryRoot: active, runsRoot: path.join(home, "state", "runs"), interfaceRoot: path.join(active, "interface"), toolRoot: path.join(active, "tool"), artifactRoot: path.join(active, "artifact") };
    const planWrites = nativeHomePlan.writes.map(write => ({ ...write, destination: write.filePath, expectedBeforeDigest: write.expectedBeforeHash, afterDigest: crypto.createHash("sha256").update(write.content).digest("hex") }));
    const nativeHome = JSON.parse(planWrites.find(write => write.tag === `run:${nativeHomePlan.runId}`).content);
    const records = check.validatePlannedHomeRuns(validationRoots, nativeHomePlan, planWrites, nativeHome);
    assert.equal(records.length, 9);
    const writer = records.find(run => run.interface_state && Object.values(run.interface_state).some(state => state.file_writer_target));
    const changedWriter = planWrites.map(write => write.tag === `run:${writer.id}` ? { ...write, content: JSON.stringify({ ...writer, trace: `${writer.trace}:changed` }) } : write);
    assert.throws(() => check.validatePlannedHomeRuns(validationRoots, nativeHomePlan, changedWriter, nativeHome), /declared initial record/);
    const extra = { ...writer, id: "run_unrelated", checkpoints: writer.checkpoints.map(cp => ({ ...cp, run: "run_unrelated" })) };
    assert.throws(() => check.validatePlannedHomeRuns(validationRoots, nativeHomePlan, [...planWrites, { destination: path.join(validationRoots.runsRoot, `${extra.id}.json`), content: JSON.stringify(extra) }], nativeHome), /undeclared Run/);
    const tampered = structuredClone(records);
    const prelude = tampered.flatMap(run => Object.values(run.interface_state ?? {})).find(state => state.prelude_sources?.length);
    prelude.prelude_sources[0].source_sha256 = "0".repeat(64);
    await assert.rejects(check.validatePlannedHomeOwnership(validationRoots, nativeHomePlan, planWrites, tampered), /snapshot differs from its exact source evidence/);
    const catalogFile = await readClientAgentCatalog(home);
    assert.equal(catalogFile.document.agents.length, 1);
    assert.equal(catalogFile.document.project_sources.length, 2);
    assert.ok(catalogFile.document.project_sources.every(source => source.source_worker_interface === taskRecipe.worker_interface_id));
    const agent = catalogFile.document.agents[0];
    const sidecarPath = path.join(active, "interface", workspace.homeInterfaceId, "octopus_os.json");
    const sidecar = JSON.parse(await fs.readFile(sidecarPath));
    const homeSidecarDigest = crypto.createHash("sha256").update(await fs.readFile(sidecarPath)).digest("hex");
    const mutableHome = await check.readMutableWorkspaceSidecar(validationRoots, { homeInterfaceId: workspace.homeInterfaceId, homeSidecarDigest }, homeSidecarDigest);
    assert.deepEqual(mutableHome.value.voice_context, nativeHome.interface_state[workspace.homeInterfaceId].voice_context);
    const profile = sidecar.task_authoring_profiles.find((entry) => entry.worker_choice_interface === agent.worker_interface);
    assert.deepEqual(profile.context_artifact_bindings, recipe.context_artifact_bindings);
    const policy = JSON.parse(await fs.readFile(path.join(active, "interface", agent.worker_interface, "policy.json")));
    const sourcePolicy = JSON.parse(await fs.readFile(path.join(active, "interface", recipe.worker_interface_id, "policy.json")));
    assert.ok(sourcePolicy.ceiling.interfaces.includes("system_memory")
      && ["memory.save", "memory.search", "memory.read", "memory.list"].every((tool) => sourcePolicy.ceiling.tools.includes(tool)),
    "installed applications enter the preset worker's ceiling with their task tools");
    assert.deepEqual(policy, { ...sourcePolicy, id: agent.worker_interface });
    const settings = JSON.parse(await fs.readFile(path.join(active, "interface", agent.worker_interface, "client_template.json")));
    assert.equal(settings.provider_interface_id, providerId);
    assert.equal(settings.reasoning_effort, effort);
    assert.deepEqual(settings.worker_environment_kinds, ["desktop_session", "code_mode", "browser_session"]);
    const task = await planners.planWorkspaceHomeSurface({ eeInstancesRoot: eeRoot, request: { chatId: "cu_actual_task_001", cycle: "cu_actual_cycle_001", eeId: agent.project_ee_id, clientSurfaceRole: "task", workerInterfaceId: agent.worker_interface, providerInterfaceId: providerId, providerModel: model, reasoningEffort: effort, doorHostSlots: profile.door_host_slots, contextArtifactBindings: profile.context_artifact_bindings, selectedWorkerEnvironmentKinds: settings.worker_environment_kinds } });
    assert.equal(task.requiredToolIds.filter((id) => id.startsWith("gui.")).length, 24);
    assert.equal(task.requiredToolIds.some((id) => id.startsWith("microphone.") || id.startsWith("speaker.")), false);
    const environments = [];
    for (const eeId of [workspace.rootEeId, agent.project_ee_id]) {
      const file = path.join(eeRoot, `${eeId}.json`); const body = await fs.readFile(file); const row = JSON.parse(body);
      environments.push({ ...row, record_path: file, record_digest: crypto.createHash("sha256").update(body).digest("hex") });
    }
    const scope = { status: "ready", root_ee_id: workspace.rootEeId, home_run_id: workspace.homeRunId, home_interface_id: workspace.homeInterfaceId, home_sidecar_digest: crypto.createHash("sha256").update(await fs.readFile(sidecarPath)).digest("hex"), agent_catalog_path: catalogFile.file, agent_catalog_digest: catalogFile.digest, agent_project_sources: catalogFile.document.project_sources, agent_presentations: catalogFile.document.agents, environments, task_authoring_profiles: sidecar.task_authoring_profiles, client_runs: [], read_only_runs: [] };
    const add = { action: "add_agent_preset", operationId: "cu_add_preset_001", home, workspace: { rootEeId: workspace.rootEeId, homeRunId: workspace.homeRunId, homeInterfaceId: workspace.homeInterfaceId, expectedHomeSidecarDigest: scope.home_sidecar_digest, expectedAgentCatalogDigest: scope.agent_catalog_digest }, projectEeId: agent.project_ee_id, ...choice };
    delete add.projectKeys;
    const result = await mutateOctopusOsWorkspace(active, home, scope, add, {
      withAgentPreset: (input, operation) => planners.withWorkspaceAgentPreset(input, eeRoot, operation),
      commitClientAuthoring: (plan) => commitClientAuthoringTransaction({ publication: roots.publication, octopusHomeRoot: home, registryRoot: active, plan: { operationId: "cu_add_commit_001", label: plan.label, writes: plan.writes.map((write) => ({ destination: write.file, expectedBeforeDigest: write.beforeDigest, content: `${JSON.stringify(write.next, null, 2)}\n`, tag: write.file, publishBarrier: write.file === plan.publishBarrierPath })), removals: [] } }),
    });
    assert.equal(result.action, "add_agent_preset");
    assert.equal((await readClientAgentCatalog(home)).document.agents.length, 2);
    await assert.rejects(mutateOctopusOsWorkspace(active, home, scope, add, {}), /unavailable/);
    const projectPath = path.join(eeRoot, `${agent.project_ee_id}.json`);
    const project = JSON.parse(await fs.readFile(projectPath));
    await writeJson(projectPath, { ...project, policy_narrowing: { allowed_interfaces: [homeRecipe.worker_interface_id] } });
    await assert.rejects(planners.withWorkspaceAgentPreset({ ...add, operationId: "cu_denied_preset_001" }, eeRoot, () => { throw new Error("Unexpected publication"); }), /worker|admi|allow|policy/i);
    assert.deepEqual(await fs.readFile(providerFile), providerBytes);
    process.stdout.write(`actual preset: root Home setup + existing Home + 24 GUI tools + exact-worker refusal passed; planner calls=${plannerCalls}\n`);
  } finally { childProcess.spawn = originalSpawn; await fs.rm(root, { recursive: true, force: true }); }
}

async function selectedFixture() {
  await assertPackMembers();
  const [mode, executable, ...extra] = process.argv.slice(2);
  if (mode === undefined) return run();
  if (mode === "--interface-imports" && executable === undefined) return assertInterfaceImportClosure();
  if (!["--bounded-agent-preset", "--topology-binary"].includes(mode) || !executable || extra.length) {
    throw new Error("Expected --bounded-agent-preset <binary> or --topology-binary <binary>");
  }
  return mode === "--bounded-agent-preset" ? boundedAgentPresetFixture(path.resolve(executable)) : run(path.resolve(executable));
}

selectedFixture().then(() => {
  process.stdout.write("Codex Pack clean-registry Home closure passed\n");
}).catch((error) => {
  process.stderr.write(`${error instanceof Error ? error.stack : String(error)}\n`);
  process.exitCode = 1;
});
