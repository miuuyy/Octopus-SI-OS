const assert = require("node:assert/strict");
const childProcess = require("node:child_process");
const crypto = require("node:crypto");
const fs = require("node:fs/promises");
const os = require("node:os");
const path = require("node:path");
const { createProjectionStore, mutateOctopusOsWorkspace } = require("../electron/projection.cjs");
const workspaceSetup = require("../electron/workspaceSetup.cjs");
const projectionAuthoring = require("../electron/clientWorkspaceAuthoring.cjs");
const { publishFiles } = require("../../registry/interface/world_operator_chat/publication.cjs");
const { fixtureFilePublisher, fixtureDirectoryPublisher, withFixturePublisher } = require("./fixturePublication.cjs");
const initializeWorkspace = options => workspaceSetup.initializeWorkspace(withFixturePublisher(options));
const mutateWorkspaceProject = options => workspaceSetup.mutateWorkspaceProject(withFixturePublisher(options));
const readWorkspaceSetupState = options => workspaceSetup.readWorkspaceSetupState(withFixturePublisher(options));
const recoverWorkspaceTransactions = options => workspaceSetup.recoverWorkspaceTransactions(withFixturePublisher(options));
const commitClientAuthoringTransaction = options => workspaceSetup.commitClientAuthoringTransaction(withFixturePublisher(options));
const commitProjectionClientAuthoring = (home, registry, request, options = {}) =>
  projectionAuthoring.commitProjectionClientAuthoring(home, registry, request, withFixturePublisher({ octopusHomeRoot: home, ...options }));

function fixtureCanonical(value) {
  if (Array.isArray(value)) return `[${value.map(fixtureCanonical).join(",")}]`;
  if (value && typeof value === "object") return `{${Object.keys(value).sort().map(key => `${JSON.stringify(key)}:${fixtureCanonical(value[key])}`).join(",")}}`;
  return JSON.stringify(value);
}

function fixtureRetirementBarrier(publication) {
  let active = false;
  return { async verifyBefore() {}, async withHeldProof(context, operation) {
    assert.equal(active, false); active = true;
    const verify = async () => {
      assert.equal(active, true);
      return { schema_version: 2, phase: "held", operation_id: context.operation_id, selection_digest: context.selection_digest,
        directory_inventory_digest: crypto.createHash("sha256").update(fixtureCanonical(context.directory_inventory)).digest("hex"),
        interface_ids: context.selection.interface_ids, originals: context.originals.map(({ file, expected_digest }) => ({ file, expected_digest })),
        retired: context.retired, ledger_boundary: context.prior_proof?.ledger_boundary ?? { complete_offset: 0, sequence: 0,
          sha256: crypto.createHash("sha256").update("").digest("hex") } };
    };
    const guard = { verify, ...Object.fromEntries(["snapshotDirectory", "publishDirectoryMoves", "reclaimDirectory"].map(method =>
      [method, async (...args) => { assert.equal(active, true); return publication[method](...args); }])) };
    try { return await operation(guard); } finally { active = false; }
  } };
}

function retirementFixtureOptions(home, publication = fixtureDirectoryPublisher(home)) {
  return { octopusHomeRoot: home, registryRoot: path.join(home, "registry"),
    publishFiles: fixtureFilePublisher(home), retirementBarrier: fixtureRetirementBarrier(publication),
    publication: Object.fromEntries(["snapshotDirectory", "publishDirectoryMoves", "reclaimDirectory"].map(method =>
      [method, () => assert.fail(`retirement escaped its held child through ${method}`)])) };
}

async function verifyDirectoryPublicationRecovery(temporary) {
  for (const fault of ["move", "tombstone_gc", "root_move", "root_gc", "foreign", "legacy", "legacy_hold", "inventory_missing", "inventory_refused"]) {
    const home = path.join(temporary, `directory-${fault}`), registry = path.join(home, "registry");
    await createFixture(home, registry);
    const run = path.join(home, "state/runs/task.json"), provider = path.join(registry, "interface/provider");
    const content = JSON.stringify({ id: "task", worker: "worker", ee: "ee", interface_state: { provider: {}, client: { provider_runtime_interface: "provider" } } });
    const hash = bytes => crypto.createHash("sha256").update(bytes).digest("hex");
    await fs.mkdir(path.dirname(run), { recursive: true }); await fs.writeFile(run, content);
    const directories = [provider, path.join(home, "interfaces/provider"), path.join(registry, "tool/owned"), path.join(registry, "artifact/owned")];
    for (const directory of directories) { await fs.mkdir(directory, { recursive: true }); await fs.writeFile(path.join(directory, "owned.json"), "{}\n"); }
    await fs.writeFile(path.join(registry, "tool/owned/tool.json"), JSON.stringify({ id: "owned", interface: "provider" }));
    await fs.writeFile(path.join(registry, "artifact/owned/artifact.json"), JSON.stringify({ id: "owned", run: "task", owner_interface: "provider" }));
    await fs.writeFile(path.join(provider, "octopus_os.json"), JSON.stringify({ schema_version: 1, workspace: "octopus_os", interface: "provider", owner_interface: "client" }));
    await fs.writeFile(path.join(provider, "code_mode.json"), JSON.stringify({ context: { mode: "persistent" } }));
    await fs.mkdir(path.join(provider, ".octopus-publication"), { mode: 0o700 });
    const operationId = `directory_${fault}_12345678`, publication = fixtureDirectoryPublisher(home), events = [];
    const interrupted = { ...publication,
      async publishDirectoryMoves(id, moves) {
        const receipts = await publication.publishDirectoryMoves(id, moves); events.push({ id, moves });
        if (["move", "foreign", "legacy", "legacy_hold"].includes(fault) && id.includes(":remove:") || fault === "root_move" && id.includes(":cleanup:")) {
          throw new Error("fixture lost directory movement verdict");
        }
        return receipts;
      },
      async reclaimDirectory(target, receipt, expected) {
        await publication.reclaimDirectory(target, receipt, expected);
        if (fault === "tombstone_gc" && receipt.operation_id.includes(":remove:") || fault === "root_gc" && receipt.operation_id.includes(":cleanup:")) {
          throw new Error("fixture lost directory reclaim verdict");
        }
      } };
    const options = retirementFixtureOptions(home, interrupted);
    let fileEffects = 0;
    if (fault.startsWith("inventory_")) {
      options.publishFiles = async writes => { fileEffects += writes.length; return fixtureFilePublisher(home)(writes); };
      if (fault === "inventory_missing") await fs.unlink(path.join(provider, "code_mode.json"));
      else options.retirementBarrier.verifyBefore = async context => {
        assert.equal(context.directory_inventory.removals.length, 2);
        assert.ok(context.directory_inventory.originals.some(original => original.file.endsWith("code_mode.json")));
        throw new Error("fixture native inventory refused before Run writes");
      };
    }
    const removals = await Promise.all(directories.map(async destination => {
      const stat = await fs.lstat(destination);
      return { destination, type: "directory", expectedDev: stat.dev, expectedIno: stat.ino, tag: destination };
    }));
    const selection = { schema_version: 1, source_run: "task", interface_ids: ["provider", "worker"], records: [{ run: "task", file: run,
      expected_digest: hash(content), worker: "worker", ee: "ee", role: "source", provider_interface: "provider", target_run: "task" }] };
    const commit = workspaceSetup.commitClientAuthoringTransaction({ ...options, plan: { operationId, label: "retirement directory recovery",
      writes: [{ destination: run, expectedBeforeDigest: hash(content), content: null, tag: "task", publishBarrier: true }], removals, retirement: selection } });
    if (fault.startsWith("inventory_")) {
      await assert.rejects(commit, fault === "inventory_missing" ? /ENOENT/ : /inventory refused before Run writes/);
      assert.equal(fileEffects, 0); assert.equal(await fs.readFile(run, "utf8"), content);
      for (const directory of directories) assert.ok(await fs.lstat(directory));
      continue;
    }
    await assert.rejects(commit, /lost directory/);
    const transaction = path.join(home, "client-state/workspace/transactions", operationId), journalFile = path.join(transaction, "journal.json");
    if (["foreign", "legacy", "legacy_hold"].includes(fault)) {
      const journal = JSON.parse(await fs.readFile(journalFile)), tombstone = path.join(transaction, journal.removals[0].tombstone);
      if (fault === "foreign") {
        await fs.writeFile(path.join(tombstone, "foreign"), "retain foreign data");
        await assert.rejects(recoverWorkspaceTransactions(retirementFixtureOptions(home)), /identity changed/);
        assert.equal(await fs.readFile(path.join(tombstone, "foreign"), "utf8"), "retain foreign data");
      } else {
        if (fault === "legacy") { journal.schema_version = 2; for (const removal of journal.removals) delete removal.publication; }
        else { journal.retirement.schema_version = 1; delete journal.retirement.directory_inventory;
          journal.retirement.proof.schema_version = 1; delete journal.retirement.proof.directory_inventory_digest; }
        await fs.writeFile(journalFile, JSON.stringify(journal));
        await assert.rejects(recoverWorkspaceTransactions(retirementFixtureOptions(home)), fault === "legacy"
          ? /Client authoring transaction journal has an unsupported schema/
          : /Task retirement journal is missing fields: directory_inventory/);
        assert.ok(await fs.lstat(tombstone));
      }
      continue;
    }
    const child = childProcess.spawnSync(process.execPath, [__filename, "directory-recover", home], { encoding: "utf8", timeout: 20000 });
    assert.equal(child.status, 0, child.stderr);
    for (const directory of directories) await assert.rejects(fs.lstat(directory), { code: "ENOENT" });
    const cleanup = JSON.parse(await fs.readFile(path.join(home, "client-state/workspace/directory-cleanup", `${operationId}.json`)));
    assert.equal(cleanup.schema_version, 4); assert.equal(cleanup.publication.reclaimed, true);
    assert.equal(cleanup.journal.retirement.proof.schema_version, 2);
    assert.deepEqual(cleanup.retirement_context.directory_inventory, cleanup.journal.retirement.directory_inventory);
    assert.equal(cleanup.retirement_context.directory_inventory.removals.length, 2);
    assert.deepEqual(cleanup.journal.removals[0].publication.moves, events[0].moves);
    assert.ok(cleanup.journal.removals.every(removal => removal.publication.reclaimed));
    assert.equal(cleanup.publication.receipts[0].movement.source, transaction);
    assert.equal((await fs.readdir(path.dirname(transaction))).length, 0);
  }
  console.log("workspace directories: same held owner, durable plans, cold receipt/reclaim replay, foreign-data and legacy-proof refusal passed");
}

async function main() {
  const temporary = await fs.mkdtemp(path.join(os.tmpdir(), "octopus-workspace-test-"));
  try {
    await verifyPublicationRecovery(temporary);
    await verifyConditionedPublicationRecovery(await fs.realpath(temporary));
    await verifyDirectoryPublicationRecovery(await fs.realpath(temporary));
    await verifyLiveWorkspaceLock(await fs.realpath(temporary));
    await verifyLinkedTaskRemoval(await fs.realpath(temporary));
    await verifyExistingRootAttachment(temporary);
    await verifyRootFirstSetup(temporary);
    await verifySetupFile();
    await verifyFreshAgentRecipe(temporary);
    await verifyPreludeSnapshotSetup(temporary);
    await verifyMaintenanceSetup(temporary);
    const fixtureHomeRoot = path.join(temporary, "home");
    const fixtureRegistryRoot = path.join(fixtureHomeRoot, "registry");
    await createFixture(fixtureHomeRoot, fixtureRegistryRoot);
    const octopusHomeRoot = await fs.realpath(fixtureHomeRoot);
    const registryRoot = await fs.realpath(fixtureRegistryRoot);
    const refusedHomeRoot = path.join(temporary, "refused-home");
    const refusedRegistryRoot = path.join(refusedHomeRoot, "registry");
    await createFixture(refusedHomeRoot, refusedRegistryRoot);
    const refusedHome = await fs.realpath(refusedHomeRoot);
    const refusedRegistry = await fs.realpath(refusedRegistryRoot);
    const refusedRequest = setupRequest("workspace_refused_12345678");
    const refusedProjectId = deterministicId("ee_project", refusedRequest.operationId, "product");
    await assert.rejects(
      initializeWorkspace({
        octopusHomeRoot: refusedHome,
        registryRoot: refusedRegistry,
        request: refusedRequest,
        planOperatorSurface: (input) => {
          if (input.request.clientSurfaceRole === "task" && input.request.eeId === refusedProjectId) {
            throw new Error("nested Project task topology refused");
          }
          return fixturePlan(refusedHome, refusedRegistry, input);
        },
      }),
      /nested Project task topology refused/,
    );
    assert.deepEqual(await fs.readdir(path.join(refusedHome, "state", "ee_instances")), ["ee_existence.json"]);
    assert.deepEqual(await fs.readdir(path.join(refusedHome, "state", "runs")), []);
    assert.deepEqual(await fs.readdir(path.join(refusedRegistry, "interface")), []);
    assert.equal((await readWorkspaceSetupState({ octopusHomeRoot: refusedHome, registryRoot: refusedRegistry })).status, "not_configured");
    const undeclaredTaskRequest = { ...refusedRequest }; delete undeclaredTaskRequest.task;
    await assert.rejects(initializeWorkspace({
      octopusHomeRoot: refusedHome, registryRoot: refusedRegistry, request: undeclaredTaskRequest,
      planOperatorSurface: () => { throw new Error("missing Task source must refuse before planning"); },
    }), /Workspace setup request.*task/);
    assert.deepEqual(await fs.readdir(path.join(refusedHome, "state", "ee_instances")), ["ee_existence.json"]);
    assert.deepEqual(await fs.readdir(path.join(refusedRegistry, "interface")), []);
    const plannerCalls = [];
    const planOperatorSurface = async (input) => {
      plannerCalls.push({
        role: input.request.clientSurfaceRole,
        eeId: input.request.eeId,
      });
      return fixturePlan(octopusHomeRoot, registryRoot, input);
    };
    const request = setupRequest("workspace_setup_12345678");
    const progress = [];
    const first = await initializeWorkspace({
      octopusHomeRoot,
      registryRoot,
      request,
      planOperatorSurface,
      onProgress: (event) => progress.push(event),
    });
    assert.equal(first.changed, true);
    assert.equal(first.action, "initialize_workspace");
    assert.equal(first.projects.length, 3);
    assert.equal(plannerCalls.length, 5);
    assert.deepEqual(plannerCalls.map((call) => call.role), ["home", "task", "task", "task", "task"]);
    assert.equal(plannerCalls[0].eeId, first.rootEeId);
    assert.deepEqual(
      plannerCalls.slice(2).map((call) => call.eeId).sort(),
      first.projects.map((project) => project.eeId).sort(),
    );
    assert.deepEqual(
      await fs.readdir(path.join(octopusHomeRoot, "state", "runs")),
      [`${first.homeRunId}.json`],
    );
    assert.equal(
      (await fs.readdir(path.join(registryRoot, "interface"))).some((entry) => entry.includes("project_preflight_")),
      false,
    );
    assert.equal(progress.at(-1).phase, "complete");
    const homeSidecarPath = path.join(registryRoot, "interface", first.homeInterfaceId, "octopus_os.json");
    const sidecar = JSON.parse(await fs.readFile(homeSidecarPath, "utf8"));
    assert.equal(sidecar.presentation.name, "Founder");
    assert.equal(sidecar.presentation.projects.length, 3);
    assert.equal(sidecar.task_authoring_profiles.length, 4);
    const rootTaskProfile = sidecar.task_authoring_profiles.find((profile) => profile.ee_id === first.rootEeId);
    assert.equal(rootTaskProfile.worker_choice_interface, request.task.workerInterfaceId);
    assert.notEqual(rootTaskProfile.worker_choice_interface, sidecar.authoring_profile.worker_choice_interface);
    assert.deepEqual(rootTaskProfile.context_artifact_bindings, request.task.contextArtifactBindings);
    assert.ok(sidecar.task_authoring_profiles.every((profile) => profile.worker_choice_interface === request.task.workerInterfaceId));
    const state = await readWorkspaceSetupState({ octopusHomeRoot, registryRoot });
    assert.equal(state.status, "configured");
    assert.equal(state.receipt.root_ee_id, first.rootEeId);
    const second = await initializeWorkspace({ octopusHomeRoot, registryRoot, request, planOperatorSurface });
    assert.equal(second.changed, false);
    assert.equal(second.receiptDigest, first.receiptDigest);
    assert.equal(plannerCalls.length, 5);
    const voiceContext = { run_id: "voice_run", control_interface_id: "voice_control", provider_id: "voice_provider", delegation_interface_id: "voice_return" };
    sidecar.voice_context = voiceContext;
    sidecar.authoring_profile.payload_artifact_bindings = [{ id: "attachment:home-context", content: Array(32).fill(9) }];
    rootTaskProfile.payload_artifact_bindings = [{ id: "attachment:project-context", content: Array(32).fill(7) }];
    rootTaskProfile.external_relays = [{ checkpoint_id: "task_exact_return", interface_id: "system_return_task_exact", outbound_interface_ids: ["world_operator_chat_exact_recipient"] }];
    await fs.writeFile(homeSidecarPath, `${JSON.stringify(sidecar, null, 2)}\n`);
    const configuredRootPath = path.join(octopusHomeRoot, "state", "ee_instances", `${first.rootEeId}.json`);
    const configuredRoot = JSON.parse(await fs.readFile(configuredRootPath, "utf8"));
    configuredRoot.config = { octopus_os: { application_defaults: ["world_provider_codex"], driver_defaults: ["git_worktree"] } };
    configuredRoot.policy_narrowing = { allowed_worker_environment_kinds: ["git_worktree"] };
    await fs.writeFile(configuredRootPath, `${JSON.stringify(configuredRoot, null, 2)}\n`);
    let projectScope = await fixtureProjectScope(octopusHomeRoot, registryRoot, first);
    await fs.writeFile(homeSidecarPath, `${JSON.stringify({ ...sidecar,
      task_authoring_profiles: sidecar.task_authoring_profiles.filter((profile) => profile.ee_id !== first.rootEeId),
    }, null, 2)}\n`);
    const missingSourceScope = await fixtureProjectScope(octopusHomeRoot, registryRoot, first);
    const missingSourceBytes = await fs.readFile(homeSidecarPath, "utf8");
    const missingSourceEes = (await fs.readdir(path.join(octopusHomeRoot, "state", "ee_instances"))).sort();
    await assert.rejects(mutateWorkspaceProject({
      octopusHomeRoot, registryRoot, scope: missingSourceScope,
      planOperatorSurface: () => { throw new Error("missing Task source must not fall back to Home"); },
      request: { action: "create_project", operationId: "project_missing_source_12345678",
        workspace: fixtureWorkspaceIdentity(missingSourceScope), parentEeId: first.rootEeId,
        expectedParentDigest: missingSourceScope.environments.find((environment) => environment.eeId === first.rootEeId).recordDigest,
        templateName: "project", name: "Missing source", goal: "Missing source", presentation: { icon: "circle", color: "gray" } },
    }), /exact Agent source profiles/);
    assert.deepEqual((await fs.readdir(path.join(octopusHomeRoot, "state", "ee_instances"))).sort(), missingSourceEes);
    assert.equal(await fs.readFile(homeSidecarPath, "utf8"), missingSourceBytes);
    await fs.writeFile(homeSidecarPath, `${JSON.stringify(sidecar, null, 2)}\n`);
    await fs.writeFile(homeSidecarPath, `${JSON.stringify({ ...sidecar, voice_context: { ...voiceContext, provider_id: "../escape" } }, null, 2)}\n`);
    const invalidVoiceScope = await fixtureProjectScope(octopusHomeRoot, registryRoot, first);
    await assert.rejects(mutateWorkspaceProject({
      octopusHomeRoot, registryRoot, scope: invalidVoiceScope,
      request: { action: "update_project_presentation", operationId: "invalid_voice_12345678", workspace: fixtureWorkspaceIdentity(invalidVoiceScope), projects: sidecar.presentation.projects.map(({ ee_id, icon, color }) => ({ eeId: ee_id, icon, color })) },
    }), /Home voice context must declare exact/);
    await fs.writeFile(homeSidecarPath, `${JSON.stringify(sidecar, null, 2)}\n`);
    const preflightEes = (await fs.readdir(path.join(octopusHomeRoot, "state", "ee_instances"))).sort();
    const preflightSidecar = await fs.readFile(path.join(registryRoot, "interface", first.homeInterfaceId, "octopus_os.json"), "utf8");
    await assert.rejects(
      mutateWorkspaceProject({
        octopusHomeRoot,
        registryRoot,
        scope: projectScope,
        planOperatorSurface: async () => {
          throw new Error("formed policy refuses the authored worker");
        },
        request: {
          action: "create_project",
          operationId: "project_refused_12345678",
          workspace: fixtureWorkspaceIdentity(projectScope),
          parentEeId: first.rootEeId,
          expectedParentDigest: projectScope.environments.find((environment) => environment.eeId === first.rootEeId).recordDigest,
          templateName: "project",
          name: "Refused",
          goal: "Refused",
          presentation: { icon: "circle", color: "gray" },
        },
      }),
      /formed policy refuses the authored worker/,
    );
    assert.deepEqual((await fs.readdir(path.join(octopusHomeRoot, "state", "ee_instances"))).sort(), preflightEes);
    assert.equal(
      await fs.readFile(path.join(registryRoot, "interface", first.homeInterfaceId, "octopus_os.json"), "utf8"),
      preflightSidecar,
    );
    const createdProject = await mutateWorkspaceProject({
      octopusHomeRoot,
      registryRoot,
      scope: projectScope,
      planOperatorSurface,
      request: {
        action: "create_project",
        operationId: "project_create_12345678",
        workspace: fixtureWorkspaceIdentity(projectScope),
        parentEeId: first.rootEeId,
        expectedParentDigest: projectScope.environments.find((environment) => environment.eeId === first.rootEeId).recordDigest,
        templateName: "project",
        name: "Research",
        goal: "Research",
        presentation: { icon: "flask", color: "blue" },
      },
    });
    assert.equal(createdProject.changed, true);
    assert.equal(createdProject.parentEeId, first.rootEeId);
    const createdRecord = JSON.parse(await fs.readFile(path.join(octopusHomeRoot, "state", "ee_instances", `${createdProject.eeId}.json`), "utf8"));
    assert.equal(createdRecord.template_name, "project");
    assert.equal(createdRecord.name, "Research");
    assert.deepEqual(createdRecord.config, { ...configuredRoot.config,
      octopus_os: { ...configuredRoot.config.octopus_os, source_folders: [] } });
    assert.equal(createdRecord.policy_narrowing, undefined);
    let updatedSidecar = JSON.parse(await fs.readFile(homeSidecarPath, "utf8"));
    assert.deepEqual(updatedSidecar.voice_context, voiceContext);
    assert.equal(updatedSidecar.presentation.projects.at(-1).ee_id, createdProject.eeId);
    assert.equal(updatedSidecar.task_authoring_profiles.at(-1).ee_id, createdProject.eeId);
    assert.equal(updatedSidecar.task_authoring_profiles.at(-1).worker_choice_interface, request.task.workerInterfaceId);
    assert.deepEqual(updatedSidecar.task_authoring_profiles.at(-1).payload_artifact_bindings, rootTaskProfile.payload_artifact_bindings);
    assert.deepEqual(updatedSidecar.task_authoring_profiles.at(-1).external_relays, rootTaskProfile.external_relays);
    const repeatedCreate = await mutateWorkspaceProject({
      octopusHomeRoot,
      registryRoot,
      scope: projectScope,
      request: {
        action: "create_project",
        operationId: "project_create_12345678",
        workspace: fixtureWorkspaceIdentity(projectScope),
        parentEeId: first.rootEeId,
        expectedParentDigest: projectScope.environments.find((environment) => environment.eeId === first.rootEeId).recordDigest,
        templateName: "project",
        name: "Research",
        goal: "Research",
        presentation: { icon: "flask", color: "blue" },
      },
    });
    assert.equal(repeatedCreate.changed, false);
    assert.equal(repeatedCreate.eeId, createdProject.eeId);
    delete updatedSidecar.presentation.name;
    await fs.writeFile(homeSidecarPath, `${JSON.stringify(updatedSidecar, null, 2)}\n`);
    projectScope = await fixtureProjectScope(octopusHomeRoot, registryRoot, first);
    const presentations = updatedSidecar.presentation.projects.map((row) => ({
      eeId: row.ee_id,
      expectedProjectDigest: projectScope.environments.find((environment) => environment.eeId === row.ee_id).recordDigest,
      name: row.ee_id === createdProject.eeId ? "Research Lab" : row.name,
      icon: row.icon,
      color: row.color,
    })).reverse();
    const receiptPath = path.join(
      octopusHomeRoot,
      "client-state",
      "workspace",
      "topology-receipts",
      "project_present_12345678.json",
    );
    const originalLink = fs.link;
    let refusedReceiptOnce = false;
    fs.link = async (source, destination) => {
      if (!refusedReceiptOnce && destination === receiptPath) {
        refusedReceiptOnce = true;
        throw new Error("injected receipt publication failure");
      }
      return originalLink(source, destination);
    };
    try {
      const recoveredPresentation = await mutateWorkspaceProject({
        octopusHomeRoot,
        registryRoot,
        scope: projectScope,
        request: {
          action: "update_project_presentation",
          operationId: "project_present_12345678",
          workspace: fixtureWorkspaceIdentity(projectScope),
          projects: presentations,
        },
      });
      assert.equal(recoveredPresentation.changed, true);
      assert.equal(refusedReceiptOnce, true);
    } finally {
      fs.link = originalLink;
    }
    assert.equal(JSON.parse(await fs.readFile(receiptPath, "utf8")).action, "update_project_presentation");
    updatedSidecar = JSON.parse(await fs.readFile(homeSidecarPath, "utf8"));
    assert.equal(updatedSidecar.presentation.name, undefined);
    assert.deepEqual(updatedSidecar.voice_context, voiceContext);
    assert.deepEqual(updatedSidecar.presentation.projects.map((row) => row.ee_id), presentations.map((row) => row.eeId));
    assert.equal(updatedSidecar.presentation.projects.find((row) => row.ee_id === createdProject.eeId).name, "Research Lab");
    const renamedRecord = JSON.parse(await fs.readFile(path.join(octopusHomeRoot, "state", "ee_instances", `${createdProject.eeId}.json`), "utf8"));
    assert.equal(renamedRecord.name, "Research Lab");
    assert.equal(renamedRecord.goal, createdRecord.goal);
    projectScope = await fixtureProjectScope(octopusHomeRoot, registryRoot, first);
    const projectDigest = projectScope.environments.find((environment) => environment.eeId === createdProject.eeId).recordDigest;
    const lateRunPath = path.join(octopusHomeRoot, "state", "runs", "run_project_delete_guard.json");
    await fs.writeFile(lateRunPath, `${JSON.stringify({
      id: "run_project_delete_guard",
      ee: createdProject.eeId,
    }, null, 2)}\n`);
    await assert.rejects(
      mutateWorkspaceProject({
        octopusHomeRoot,
        registryRoot,
        scope: projectScope,
        request: {
          action: "delete_project",
          operationId: "project_delete_12345678",
          workspace: fixtureWorkspaceIdentity(projectScope),
          eeId: createdProject.eeId,
          expectedProjectDigest: projectDigest,
        },
      }),
      /is still referenced by run 'run_project_delete_guard'/,
    );
    await fs.unlink(lateRunPath);
    const deletedProject = await mutateWorkspaceProject({
      octopusHomeRoot,
      registryRoot,
      scope: projectScope,
      request: {
        action: "delete_project",
        operationId: "project_delete_12345678",
        workspace: fixtureWorkspaceIdentity(projectScope),
        eeId: createdProject.eeId,
        expectedProjectDigest: projectDigest,
      },
    });
    assert.equal(deletedProject.changed, true);
    await assert.rejects(fs.readFile(path.join(octopusHomeRoot, "state", "ee_instances", `${createdProject.eeId}.json`)), /ENOENT/);
    updatedSidecar = JSON.parse(await fs.readFile(path.join(registryRoot, "interface", first.homeInterfaceId, "octopus_os.json"), "utf8"));
    assert.equal(updatedSidecar.presentation.projects.some((row) => row.ee_id === createdProject.eeId), false);
    assert.equal(updatedSidecar.task_authoring_profiles.some((row) => row.ee_id === createdProject.eeId), false);
    assert.deepEqual(updatedSidecar.voice_context, voiceContext);
    const repeatedDelete = await mutateWorkspaceProject({
      octopusHomeRoot,
      registryRoot,
      scope: projectScope,
      request: {
        action: "delete_project",
        operationId: "project_delete_12345678",
        workspace: fixtureWorkspaceIdentity(projectScope),
        eeId: createdProject.eeId,
        expectedProjectDigest: projectDigest,
      },
    });
    assert.equal(repeatedDelete.changed, false);
    const externalEeId = "ee_created_outside_client";
    const externalEePath = path.join(octopusHomeRoot, "state", "ee_instances", `${externalEeId}.json`);
    const rootRecord = JSON.parse(await fs.readFile(path.join(octopusHomeRoot, "state", "ee_instances", `${first.rootEeId}.json`), "utf8"));
    await fs.writeFile(externalEePath, JSON.stringify({ ...rootRecord, ee_id: externalEeId, parent_ee_id: first.rootEeId, template_name: "project", goal: "Model-created goal" }));
    const retainedSidecar = await fs.readFile(homeSidecarPath, "utf8");
    const agentCatalogFile = path.join(octopusHomeRoot, "client", "agents", "catalog.json");
    const retainedCatalog = await fs.readFile(agentCatalogFile, "utf8");
    projectScope = await fixtureProjectScope(octopusHomeRoot, registryRoot, first);
    const externalDeleted = await mutateWorkspaceProject({
      octopusHomeRoot, registryRoot, scope: projectScope,
      request: { action: "delete_project", operationId: "external_delete_12345678", workspace: fixtureWorkspaceIdentity(projectScope),
        eeId: externalEeId, expectedProjectDigest: projectScope.environments.find((ee) => ee.eeId === externalEeId).recordDigest },
    });
    assert.equal(externalDeleted.changed, true);
    await assert.rejects(fs.access(externalEePath), { code: "ENOENT" });
    assert.equal(await fs.readFile(homeSidecarPath, "utf8"), retainedSidecar);
    assert.equal(await fs.readFile(agentCatalogFile, "utf8"), retainedCatalog);
    assert.equal((await readWorkspaceSetupState({ octopusHomeRoot, registryRoot })).status, "configured");
    await assert.rejects(
      initializeWorkspace({
        octopusHomeRoot,
        registryRoot,
        request: setupRequest("workspace_second_12345678"),
        planOperatorSurface,
      }),
      /already configured receipt root/,
    );
    const homeDescriptorPath = path.join(registryRoot, "interface", first.homeInterfaceId, "interface.json");
    const homeDescriptor = await fs.readFile(homeDescriptorPath);
    const symlinkTarget = path.join(temporary, "symlink-interface.json");
    await fs.writeFile(symlinkTarget, homeDescriptor);
    await fs.unlink(homeDescriptorPath);
    await fs.symlink(symlinkTarget, homeDescriptorPath);
    const symlinkState = await readWorkspaceSetupState({ octopusHomeRoot, registryRoot });
    assert.equal(symlinkState.status, "degraded");
    assert.match(symlinkState.errors.join("\n"), /ELOOP|symbolic link/i);
    await fs.unlink(homeDescriptorPath);
    await fs.writeFile(homeDescriptorPath, homeDescriptor);

    const progressFailureHome = path.join(temporary, "progress-failure-home");
    const progressFailureRegistry = path.join(progressFailureHome, "registry");
    await createFixture(progressFailureHome, progressFailureRegistry);
    const progressFailureHomeRoot = await fs.realpath(progressFailureHome);
    const progressFailureRegistryRoot = await fs.realpath(progressFailureRegistry);
    const progressFailure = await initializeWorkspace({
      octopusHomeRoot: progressFailureHomeRoot,
      registryRoot: progressFailureRegistryRoot,
      request: setupRequest("workspace_progress_12345678"),
      planOperatorSurface: (input) => fixturePlan(progressFailureHomeRoot, progressFailureRegistryRoot, input),
      onProgress: (event) => {
        if (event.phase === "complete") throw new Error("detached progress listener");
      },
    });
    assert.equal(progressFailure.changed, true);
    assert.equal(
      JSON.parse(await fs.readFile(path.join(progressFailureHomeRoot, "state", "runs", `${progressFailure.homeRunId}.json`), "utf8")).id,
      progressFailure.homeRunId,
    );
    assert.equal(
      (await readWorkspaceSetupState({ octopusHomeRoot: progressFailureHomeRoot, registryRoot: progressFailureRegistryRoot })).status,
      "configured",
    );

    const abandonedStageRoot = path.join(
      progressFailureHomeRoot,
      "client-state",
      "workspace",
      "transactions",
      ".stage-workspace_stage_12345678-0123456789abcdef",
    );
    await fs.mkdir(abandonedStageRoot);
    assert.equal(
      (await readWorkspaceSetupState({ octopusHomeRoot: progressFailureHomeRoot, registryRoot: progressFailureRegistryRoot })).status,
      "configured",
    );
    await assert.rejects(fs.lstat(abandonedStageRoot), /ENOENT/);
    const mutableRootPath = path.join(
      progressFailureHomeRoot,
      "state",
      "ee_instances",
      `${progressFailure.rootEeId}.json`,
    );
    const mutableRoot = JSON.parse(await fs.readFile(mutableRootPath, "utf8"));
    mutableRoot.goal = "A legitimately edited operator goal";
    await fs.writeFile(mutableRootPath, `${JSON.stringify(mutableRoot, null, 2)}\n`);
    assert.equal(
      (await readWorkspaceSetupState({
        octopusHomeRoot: progressFailureHomeRoot,
        registryRoot: progressFailureRegistryRoot,
      })).status,
      "configured",
    );

    const conflictFixtureHome = path.join(temporary, "conflict-home");
    const conflictFixtureRegistry = path.join(conflictFixtureHome, "registry");
    await createFixture(conflictFixtureHome, conflictFixtureRegistry);
    const conflictHomeRoot = await fs.realpath(conflictFixtureHome);
    const conflictRegistryRoot = await fs.realpath(conflictFixtureRegistry);
    const conflictRequest = setupRequest("workspace_conflict_12345678");
    let injectConflict = true;
    const conflictPlanner = async (input) => {
      const plan = await fixturePlan(conflictHomeRoot, conflictRegistryRoot, input);
      if (!injectConflict) return plan;
      injectConflict = false;
      const conflictPath = path.join(conflictRegistryRoot, "interface", plan.chatInterfaceId, "conflict.json");
      await fs.mkdir(path.dirname(conflictPath), { recursive: true });
      await fs.writeFile(conflictPath, "foreign\n");
      plan.writes.splice(1, 0, write(conflictPath, '{"planned":true}\n', "conflict"));
      return plan;
    };
    await assert.rejects(
      initializeWorkspace({
        octopusHomeRoot: conflictHomeRoot,
        registryRoot: conflictRegistryRoot,
        request: conflictRequest,
        planOperatorSurface: conflictPlanner,
      }),
      /expected an absent file, found/,
    );
    assert.deepEqual(await fs.readdir(path.join(conflictHomeRoot, "state", "ee_instances")), ["ee_existence.json"]);
    assert.equal(JSON.parse(await fs.readFile(path.join(conflictHomeRoot, "state", "ee_instances", "ee_existence.json"))).goal, "existence");
    const transactionEntries = await fs.readdir(path.join(conflictHomeRoot, "client-state", "workspace", "transactions"));
    assert.deepEqual(transactionEntries, []);

    const driftFixtureHome = path.join(temporary, "plan-drift-home");
    const driftFixtureRegistry = path.join(driftFixtureHome, "registry");
    await createFixture(driftFixtureHome, driftFixtureRegistry);
    const driftHomeRoot = await fs.realpath(driftFixtureHome);
    const driftRegistryRoot = await fs.realpath(driftFixtureRegistry);
    await assert.rejects(
      initializeWorkspace({
        octopusHomeRoot: driftHomeRoot,
        registryRoot: driftRegistryRoot,
        request: setupRequest("workspace_plan_drift_12345678"),
        planOperatorSurface: async (input) => {
          const plan = await fixturePlan(driftHomeRoot, driftRegistryRoot, input);
          plan.authoringProfile.external_inputs = [];
          return plan;
        },
      }),
      /changed the requested authoring profile/,
    );

    await assert.rejects(
      initializeWorkspace({
        octopusHomeRoot: driftHomeRoot,
        registryRoot: driftRegistryRoot,
        request: setupRequest("workspace_owner_drift_12345678"),
        planOperatorSurface: async (input) => {
          const plan = await fixturePlan(driftHomeRoot, driftRegistryRoot, input);
          const foreignOwner = "foreign_interface";
          plan.requiredInterfaceIds.push(foreignOwner);
          plan.requiredInterfaceIds.sort();
          plan.writes.push(write(
            path.join(driftRegistryRoot, "interface", foreignOwner, "interface.json"),
            `${JSON.stringify({ id: foreignOwner })}\n`,
            `interface:${foreignOwner}`,
          ));
          return plan;
        },
      }),
      /outside its active Home bindings or worker ceiling/,
    );
    for (const defect of ["disconnected", "inactive", "foreign-tool-door", "missing-descriptor"]) {
      await assert.rejects(initializeWorkspace({
        octopusHomeRoot: driftHomeRoot, registryRoot: driftRegistryRoot,
        request: setupRequest(`workspace_${defect.replaceAll("-", "_")}_12345678`),
        planOperatorSurface: async (input) => {
          const plan = await fixturePlan(driftHomeRoot, driftRegistryRoot, input);
          const runWrite = plan.writes.find((row) => row.tag.startsWith("run:"));
          const run = JSON.parse(runWrite.content);
          const intake = run.checkpoints.filter((row) => row.id.startsWith("cp_intake_"));
          const service = intake[0].interface_binding.interface;
          if (defect === "disconnected") intake[0].outbound_interface_bindings = intake[1].outbound_interface_bindings;
          if (defect === "inactive") run.active_checkpoints = run.active_checkpoints.filter((id) => !intake.some((row) => row.id === id));
          if (defect === "foreign-tool-door") {
            const tool = plan.writes.find((row) => row.tag.startsWith("tool:file.write.input_"));
            tool.content = JSON.stringify({ ...JSON.parse(tool.content), interface: "foreign_interface" });
          }
          if (defect === "missing-descriptor") plan.writes = plan.writes.filter((row) => row.filePath !== path.join(driftRegistryRoot, "interface", service, "interface.json"));
          runWrite.content = JSON.stringify(run);
          return plan;
        },
      }), /outside its active Home bindings or worker ceiling|omitted exact interface descriptor/);
      assert.deepEqual(await fs.readdir(path.join(driftHomeRoot, "state/runs")), []);
    }
    for (const defect of ["unbound", "unreadable", "wrong-tag", "non-object", "wrong-file", "changed-before"]) {
      await assert.rejects(initializeWorkspace({
        octopusHomeRoot: driftHomeRoot, registryRoot: driftRegistryRoot, request: setupRequest(`workspace_artifact_${defect.replaceAll("-", "_")}`),
        planOperatorSurface: async (input) => {
          const plan = await fixturePlan(driftHomeRoot, driftRegistryRoot, input);
          if (input.request.clientSurfaceRole !== "home") return plan;
          const artifact = plan.writes.find((write) => write.tag.startsWith("artifact:"));
          const runWrite = plan.writes.find((write) => write.tag.startsWith("run:")), run = JSON.parse(runWrite.content);
          if (defect === "unbound") run.checkpoints[0].artifact_bindings = [];
          if (defect === "unreadable") run.checkpoints[0].artifact_bindings[0].access = { read: false };
          if (defect === "wrong-tag") artifact.tag = "artifact:foreign";
          if (defect === "non-object") artifact.content = "[]";
          if (defect === "wrong-file") artifact.filePath = path.join(path.dirname(artifact.filePath), "other.json");
          if (defect === "changed-before") artifact.expectedBeforeHash = "a".repeat(64);
          runWrite.content = JSON.stringify(run);
          return plan;
        },
      }), /exact readable checkpoint binding|exact owner|artifact.json|absent fresh-workspace file|JSON object/);
      assert.deepEqual(await fs.readdir(path.join(driftRegistryRoot, "artifact")), []);
    }

    const orphanFixtureHome = path.join(temporary, "orphan-home");
    const orphanFixtureRegistry = path.join(orphanFixtureHome, "registry");
    await createFixture(orphanFixtureHome, orphanFixtureRegistry);
    const orphanHome = await fs.realpath(orphanFixtureHome);
    const orphanRegistry = await fs.realpath(orphanFixtureRegistry);
    await fs.writeFile(path.join(orphanHome, "state", "ee_instances", "ee_orphan_space.json"), `${JSON.stringify({
      ee_id: "ee_orphan_space",
      template_name: "operator_space",
      parent_ee_id: "ee_existence",
      goal: "Orphaned workspace",
      config: {},
    }, null, 2)}\n`);
    const orphanState = await readWorkspaceSetupState({ octopusHomeRoot: orphanHome, registryRoot: orphanRegistry });
    assert.equal(orphanState.status, "degraded");
    assert.match(orphanState.errors.join("\n"), /operator spaces without receipts/);
    await assert.rejects(
      initializeWorkspace({
        octopusHomeRoot: orphanHome,
        registryRoot: orphanRegistry,
        request: setupRequest("workspace_orphan_12345678"),
        planOperatorSurface: (input) => fixturePlan(orphanHome, orphanRegistry, input),
      }),
      /refuses existing operator space/,
    );

    const crashFixtureHome = path.join(temporary, "crash-home");
    const crashFixtureRegistry = path.join(crashFixtureHome, "registry");
    await createFixture(crashFixtureHome, crashFixtureRegistry);
    const crashHome = await fs.realpath(crashFixtureHome);
    const crashRegistry = await fs.realpath(crashFixtureRegistry);
    const child = childProcess.spawnSync(process.execPath, [__filename, "crash-child", crashHome, crashRegistry], {
      encoding: "utf8",
    });
    assert.equal(child.status, 73, child.stderr || child.stdout);
    const recoveredState = await readWorkspaceSetupState({ octopusHomeRoot: crashHome, registryRoot: crashRegistry });
    assert.equal(recoveredState.status, "configured");
    assert.equal(recoveredState.receipt.operation_id, "workspace_crash_12345678");
    assert.equal(recoveredState.receipt.root_ee_id, "ee_existence");
    assert.equal(JSON.parse(await fs.readFile(path.join(crashHome, "state", "ee_instances", "ee_existence.json"))).goal, "Operate life and work");
    assert.equal((await fs.readdir(path.join(crashHome, "client-state", "workspace", "transactions"))).length, 0);

    const projectCrashFixtureHome = path.join(temporary, "project-crash-home");
    const projectCrashFixtureRegistry = path.join(projectCrashFixtureHome, "registry");
    await createFixture(projectCrashFixtureHome, projectCrashFixtureRegistry);
    const projectCrashHome = await fs.realpath(projectCrashFixtureHome);
    const projectCrashRegistry = await fs.realpath(projectCrashFixtureRegistry);
    const projectCrashWorkspace = await initializeWorkspace({
      octopusHomeRoot: projectCrashHome,
      registryRoot: projectCrashRegistry,
      request: setupRequest("workspace_project_crash_12345678"),
      planOperatorSurface: (input) => fixturePlan(projectCrashHome, projectCrashRegistry, input),
    });
    const projectCrashArgs = [
      projectCrashHome,
      projectCrashRegistry,
      projectCrashWorkspace.rootEeId,
      projectCrashWorkspace.homeRunId,
      projectCrashWorkspace.homeInterfaceId,
    ];
    const createCrash = childProcess.spawnSync(
      process.execPath,
      [__filename, "project-crash-child", "create", ...projectCrashArgs],
      { encoding: "utf8" },
    );
    assert.equal(createCrash.status, 73, createCrash.stderr || createCrash.stdout);
    const crashCreatedEeId = deterministicId("ee_project", "project_crash_create_12345678", "project");
    assert.equal(
      JSON.parse(await fs.readFile(path.join(projectCrashHome, "state", "ee_instances", `${crashCreatedEeId}.json`), "utf8")).ee_id,
      crashCreatedEeId,
    );
    let projectCrashSidecar = JSON.parse(await fs.readFile(
      path.join(projectCrashRegistry, "interface", projectCrashWorkspace.homeInterfaceId, "octopus_os.json"),
      "utf8",
    ));
    assert.equal(projectCrashSidecar.task_authoring_profiles.some((profile) => profile.ee_id === crashCreatedEeId), false);
    assert.equal(projectCrashSidecar.presentation.projects.some((projectRow) => projectRow.ee_id === crashCreatedEeId), false);
    let projectCrashState = await readWorkspaceSetupState({
      octopusHomeRoot: projectCrashHome,
      registryRoot: projectCrashRegistry,
    });
    assert.equal(projectCrashState.status, "configured");
    assert.equal(
      JSON.parse(await fs.readFile(path.join(projectCrashHome, "state", "ee_instances", `${crashCreatedEeId}.json`), "utf8")).ee_id,
      crashCreatedEeId,
    );
    projectCrashSidecar = JSON.parse(await fs.readFile(
      path.join(projectCrashRegistry, "interface", projectCrashWorkspace.homeInterfaceId, "octopus_os.json"),
      "utf8",
    ));
    assert.equal(projectCrashSidecar.task_authoring_profiles.filter((profile) => profile.ee_id === crashCreatedEeId).length, 1);
    assert.equal(projectCrashSidecar.presentation.projects.filter((projectRow) => projectRow.ee_id === crashCreatedEeId).length, 1);
    assert.equal(
      JSON.parse(await fs.readFile(path.join(
        projectCrashHome,
        "client-state",
        "workspace",
        "topology-receipts",
        "project_crash_create_12345678.json",
      ), "utf8")).action,
      "create_project",
    );
    assert.deepEqual(await fs.readdir(path.join(projectCrashHome, "client-state", "workspace", "transactions")), []);

    const historicalProject = projectCrashWorkspace.projects.find((entry) => entry.key === "life");
    assert.ok(historicalProject);
    const deleteCrash = childProcess.spawnSync(
      process.execPath,
      [__filename, "project-crash-child", "delete", ...projectCrashArgs, historicalProject.eeId],
      { encoding: "utf8" },
    );
    assert.equal(deleteCrash.status, 73, deleteCrash.stderr || deleteCrash.stdout);
    assert.equal(
      JSON.parse(await fs.readFile(path.join(projectCrashHome, "state", "ee_instances", `${historicalProject.eeId}.json`), "utf8")).ee_id,
      historicalProject.eeId,
    );
    projectCrashSidecar = JSON.parse(await fs.readFile(
      path.join(projectCrashRegistry, "interface", projectCrashWorkspace.homeInterfaceId, "octopus_os.json"),
      "utf8",
    ));
    assert.equal(projectCrashSidecar.task_authoring_profiles.some((profile) => profile.ee_id === historicalProject.eeId), false);
    assert.equal(projectCrashSidecar.presentation.projects.some((projectRow) => projectRow.ee_id === historicalProject.eeId), false);
    projectCrashState = await readWorkspaceSetupState({
      octopusHomeRoot: projectCrashHome,
      registryRoot: projectCrashRegistry,
    });
    assert.equal(projectCrashState.status, "configured");
    assert.equal(
      projectCrashState.receipt.project_ee_ids.some((entry) => entry.ee_id === historicalProject.eeId),
      true,
    );
    await assert.rejects(
      fs.readFile(path.join(projectCrashHome, "state", "ee_instances", `${historicalProject.eeId}.json`)),
      /ENOENT/,
    );
    projectCrashSidecar = JSON.parse(await fs.readFile(
      path.join(projectCrashRegistry, "interface", projectCrashWorkspace.homeInterfaceId, "octopus_os.json"),
      "utf8",
    ));
    assert.equal(projectCrashSidecar.task_authoring_profiles.some((profile) => profile.ee_id === historicalProject.eeId), false);
    assert.equal(projectCrashSidecar.presentation.projects.some((projectRow) => projectRow.ee_id === historicalProject.eeId), false);
    assert.equal(
      JSON.parse(await fs.readFile(path.join(
        projectCrashHome,
        "client-state",
        "workspace",
        "topology-receipts",
        "project_crash_delete_12345678.json",
      ), "utf8")).action,
      "delete_project",
    );
    assert.deepEqual(await fs.readdir(path.join(projectCrashHome, "client-state", "workspace", "transactions")), []);

    const mismatchFixtureHome = path.join(temporary, "journal-mismatch-home");
    const mismatchFixtureRegistry = path.join(mismatchFixtureHome, "registry");
    await createFixture(mismatchFixtureHome, mismatchFixtureRegistry);
    const mismatchHome = await fs.realpath(mismatchFixtureHome);
    const mismatchRegistry = await fs.realpath(mismatchFixtureRegistry);
    const mismatchChild = childProcess.spawnSync(
      process.execPath,
      [__filename, "crash-child", mismatchHome, mismatchRegistry],
      { encoding: "utf8" },
    );
    assert.equal(mismatchChild.status, 73, mismatchChild.stderr || mismatchChild.stdout);
    const mismatchJournalPath = path.join(
      mismatchHome,
      "client-state",
      "workspace",
      "transactions",
      "workspace_crash_12345678",
      "journal.json",
    );
    const mismatchJournal = JSON.parse(await fs.readFile(mismatchJournalPath, "utf8"));
    mismatchJournal.receipt.writes[0].digest = "0".repeat(64);
    await fs.writeFile(mismatchJournalPath, `${JSON.stringify(mismatchJournal, null, 2)}\n`);
    const mismatchState = await readWorkspaceSetupState({
      octopusHomeRoot: mismatchHome,
      registryRoot: mismatchRegistry,
    });
    assert.equal(mismatchState.status, "degraded");
    assert.match(mismatchState.errors.join("\n"), /receipt disagrees with its journal writes/);

    const driftedRootPath = mutableRootPath;
    await fs.writeFile(driftedRootPath, "{}\n");
    const driftedState = await readWorkspaceSetupState({
      octopusHomeRoot: progressFailureHomeRoot,
      registryRoot: progressFailureRegistryRoot,
    });
    assert.equal(driftedState.status, "degraded");
    assert.equal(driftedState.receipt, null);
    assert.match(driftedState.errors.join("\n"), /root EE identity/);
    await verifyOperatorChatStatusProjection(temporary);
    await verifyTaskArchive(temporary);
    await verifyProjectionReadinessBarrier(temporary);
  } finally {
    await fs.rm(temporary, { recursive: true, force: true });
  }
}

async function verifyPublicationRecovery(temporary) {
  const fixture = path.join(temporary, "publication-recovery");
  await createFixture(fixture, path.join(fixture, "registry"));
  const home = await fs.realpath(fixture), registry = path.join(home, "registry"), target = path.join(home, "state/runs/publication.json");
  const before = '{"before":true}\n', after = '{"after":"café"}\n', hash = bytes => crypto.createHash("sha256").update(bytes).digest("hex");
  await fs.mkdir(path.dirname(target), { recursive: true }); await fs.writeFile(target, before);
  const options = { octopusHomeRoot: home, registryRoot: registry, plan: { operationId: "publication_recovery_12345678", label: "publication recovery",
    removals: [], writes: [{ destination: target, expectedBeforeDigest: hash(before), content: after, tag: "publication", publishBarrier: true }] } };
  assert.equal((await workspaceSetup.readWorkspaceSetupState({ octopusHomeRoot: home, registryRoot: registry,
    publishFiles: () => assert.fail("read without a transaction invoked native publication") })).status, "not_configured");
  await assert.rejects(workspaceSetup.commitClientAuthoringTransaction(options), /native file publisher/);
  assert.equal(await fs.readFile(target, "utf8"), before);
  const publisher = fixtureFilePublisher(home), operations = [];
  const loseReceipt = async writes => { operations.push(writes[0].operationId); await publisher(writes); throw new Error("fixture lost native receipt"); };
  await assert.rejects(commitClientAuthoringTransaction({ ...options, publishFiles: loseReceipt }), /lost native receipt/);
  const transaction = path.join(home, "client-state/workspace/transactions", options.plan.operationId), journalPath = path.join(transaction, "journal.json");
  const retained = await fs.readFile(journalPath, "utf8"), journal = JSON.parse(retained);
  assert.equal(journal.schema_version, 3); assert.equal(journal.writes[0].publication.receipt, null);
  assert.equal(new Set(operations).size, 1); assert.equal(await fs.readFile(target, "utf8"), after);
  const snapshots = await Promise.all([journal.writes[0].before_file, journal.writes[0].after_file].map(file => fs.readFile(path.join(transaction, file))));
  const legacy = structuredClone(journal); legacy.schema_version = 1; delete legacy.writes[0].publication;
  await fs.writeFile(journalPath, JSON.stringify(legacy));
  await assert.rejects(recoverWorkspaceTransactions({ ...options, publishFiles: () => assert.fail("legacy postimage invoked publication") }),
    /unsupported schema/);
  assert.equal(await fs.readFile(target, "utf8"), after);
  const advanced = '{"current":"next run checkpoint"}\n';
  await fs.writeFile(target, advanced);
  await fs.writeFile(journalPath, retained); await recoverWorkspaceTransactions({ ...options, publishFiles: publisher });
  assert.equal(await fs.readFile(target, "utf8"), advanced);
  await commitClientAuthoringTransaction({ ...options, publishFiles: () => assert.fail("completed operation was republished") });
  assert.equal(await fs.readFile(target, "utf8"), advanced);
  const receipt = JSON.parse(await fs.readFile(path.join(home, "client-state/workspace/client-authoring-receipts", options.plan.operationId + ".json")));
  assert.equal(receipt.publication_evidence.writes[0].publication.receipt.operation_id, operations[0]);
  assert.equal(receipt.publication_evidence.writes[0].publication.receipt.outcome, "applied");
  legacy.operation_id = "publication_legacy_12345678"; legacy.writes[0].state = "applied";
  const legacyRoot = path.join(path.dirname(transaction), legacy.operation_id);
  for (const [index, file] of [legacy.writes[0].before_file, legacy.writes[0].after_file].entries()) {
    await fs.mkdir(path.dirname(path.join(legacyRoot, file)), { recursive: true }); await fs.writeFile(path.join(legacyRoot, file), snapshots[index]);
  }
  await fs.writeFile(path.join(legacyRoot, "journal.json"), JSON.stringify(legacy));
  await assert.rejects(recoverWorkspaceTransactions({ ...options, publishFiles: () => assert.fail("legacy applied write invoked publication") }), /unsupported schema/);
  assert.equal(await fs.readFile(target, "utf8"), advanced);

  const rollbackFixture = path.join(temporary, "publication-rollback");
  await createFixture(rollbackFixture, path.join(rollbackFixture, "registry"));
  const rollbackHome = await fs.realpath(rollbackFixture), rollbackRegistry = path.join(rollbackHome, "registry");
  const rollbackPublisher = fixtureFilePublisher(rollbackHome), calls = [];
  await assert.rejects(initializeWorkspace({ octopusHomeRoot: rollbackHome, registryRoot: rollbackRegistry,
    request: setupRequest("publication_rollback_12345678"), planOperatorSurface: input => fixturePlan(rollbackHome, rollbackRegistry, input),
    publishFiles: async writes => {
      calls.push(writes[0]);
      if (calls.length !== 2) return rollbackPublisher(writes);
      return publishFiles(writes, async ({ writes: requests }) => ({ receipts: requests.map(write => ({ digest_algorithm: "sha256",
        operation_id: write.operation_id, path: write.path, before_hash: write.expected_before_hash, after_hash: hash(write.content),
        bytes: Buffer.byteLength(write.content), outcome: "not_applied", error: "fixture publication refusal" })), error: null }));
    } }), /fixture publication refusal/);
  assert.equal(calls.length, 3); assert.notEqual(calls[2].operationId, calls[0].operationId);
  assert.equal(calls[2].expectedBeforeHash, hash(calls[0].content)); assert.equal(hash(calls[2].content), calls[0].expectedBeforeHash);
  const rollbackFile = (await fs.readdir(path.join(rollbackHome, "client-state/workspace"))).find(file => file.startsWith("rollback-publication_rollback_"));
  const rollback = JSON.parse(await fs.readFile(path.join(rollbackHome, "client-state/workspace", rollbackFile)));
  assert.equal(rollback.writes[0].publication.rollback_receipt.operation_id, calls[2].operationId);
  assert.equal(rollback.writes[0].publication.rollback_receipt.outcome, "applied");
  console.log("workspace publication: retained recovery preserves later changes, obsolete journals refuse, checked rollback passed");
}

async function verifyConditionedPublicationRecovery(temporary) {
  const root = path.join(temporary, "conditioned");
  await createFixture(root, path.join(root, "registry"));
  const home = await fs.realpath(root), registry = path.join(home, "registry"), dependency = path.join(home, "state/ee_instances/authority.json");
  const files = ["first", "second"].map(name => path.join(home, "state/runs", name + ".json"));
  const before = "original\n", after = "updated\n", hash = text => crypto.createHash("sha256").update(text).digest("hex");
  await fs.mkdir(path.dirname(files[0]), { recursive: true });
  await fs.writeFile(dependency, "allowed"); for (const file of files) await fs.writeFile(file, before);
  const condition = { octopus_home: home, preimages: [{ path: dependency, sha256: hash("allowed") }, ...files.map(path => ({ path, sha256: hash(before) }))] };
  const request = { label: "conditioned adoption", condition, removals: [], publishBarrierPath: files[1],
    writes: files.map(file => ({ file, content: after, beforeDigest: hash(before), label: file })) };
  const publisher = fixtureFilePublisher(home), submissions = [];
  await assert.rejects(commitProjectionClientAuthoring(home, registry, request, {
    publishFiles: async (writes, saved) => {
      assert.deepEqual(saved, condition); submissions.push(writes.map(write => write.operationId));
      assert.equal(writes.length, 2); await fs.writeFile(dependency, "narrowed"); return publisher(writes, saved);
    },
  }), /dependency changed/);
  assert.deepEqual(await Promise.all(files.map(file => fs.readFile(file, "utf8"))), [before, before]);
  const transactions = path.join(home, "client-state/workspace/transactions"), [id] = await fs.readdir(transactions);
  const journalPath = path.join(transactions, id, "journal.json"), journal = JSON.parse(await fs.readFile(journalPath));
  assert.equal(journal.schema_version, 4); assert.deepEqual(journal.condition, condition);
  assert.equal(journal.writes.every(write => write.publication.receipt === null), true);
  await fs.writeFile(dependency, "allowed");
  await assert.rejects(recoverWorkspaceTransactions({ octopusHomeRoot: home, registryRoot: registry,
    publishFiles: async (writes, saved) => {
      submissions.push(writes.map(write => write.operationId)); await publisher(writes, saved);
      await fs.writeFile(dependency, "changed after original effects"); throw new Error("lost conditioned native verdict");
    },
  }), /lost conditioned native verdict/);
  await fs.writeFile(path.join(home, "state/fixture-runtime-owned"), "held");
  const child = childProcess.spawn(process.execPath, [__filename, "conditioned-recover", home], { stdio: ["ignore", "pipe", "pipe"] });
  let output = ""; child.stderr.on("data", bytes => { output += bytes; });
  const exit = await new Promise(resolve => child.once("exit", resolve)); assert.equal(exit, 0, output);
  const receipt = JSON.parse(await fs.readFile(path.join(home, "client-state/workspace/client-authoring-receipts", id + ".json")));
  assert.deepEqual(receipt.publication_evidence.condition, condition);
  assert.deepEqual(receipt.publication_evidence.writes.map(write => write.publication.receipt.operation_id), submissions[0]);
  assert.ok(submissions.every(ids => JSON.stringify(ids) === JSON.stringify(submissions[0])));
  assert.deepEqual(await Promise.all(files.map(file => fs.readFile(file, "utf8"))), [after, after]);
  await fs.unlink(path.join(home, "state/fixture-runtime-owned"));
  await fs.writeFile(dependency, "allowed"); for (const file of files) await fs.writeFile(file, before);
  let rollback = 0;
  await assert.rejects(commitProjectionClientAuthoring(home, registry, request, { publishFiles: async (writes, saved) => {
    if (writes.length === 1) { assert.deepEqual(saved, { octopus_home: home, preimages: [] }); rollback++; return publisher(writes, saved); }
    const [applied] = await publisher([writes[0]], saved);
    const refused = { digest_algorithm: "sha256", operation_id: writes[1].operationId, path: files[1], before_hash: hash(before),
      after_hash: hash(after), bytes: Buffer.byteLength(after), outcome: "not_applied", error: "fixture native refusal" };
    await fs.writeFile(dependency, "narrowed before undo");
    const failure = new Error(refused.error); failure.publicationReceipts = [applied, refused]; throw failure;
  } }), /fixture native refusal/);
  assert.equal(rollback, 1); assert.deepEqual(await Promise.all(files.map(file => fs.readFile(file, "utf8"))), [before, before]);
  console.log("conditioned publication: zero-effect dependency refusal, durable full batch, cold original receipt replay and stopped checked undo passed");
}

async function verifyPreludeSnapshotSetup(temporary) {
  const home = path.join(await fs.realpath(temporary), "prelude-snapshot-setup"), registry = path.join(home, "registry");
  await createFixture(home, registry);
  const { blake3 } = require("@noble/hashes/blake3");
  const canonical = (value) => JSON.stringify(value, function (_key, entry) {
    return entry && !Array.isArray(entry) && typeof entry === "object"
      ? Object.fromEntries(Object.keys(entry).sort().map((key) => [key, entry[key]])) : entry;
  });
  const pin = (value) => [...blake3(Buffer.from(canonical(value)))];
  const sourceId = "artifact-code-mode-browser-prelude", snapshotId = `${sourceId}__source__setup`;
  const sourceBytes = await fs.readFile(path.join(__dirname, "../../registry/artifact", sourceId, "artifact.json"));
  const sourcePath = path.join(registry, "artifact", sourceId, "artifact.json"), snapshotPath = path.join(registry, "artifact", snapshotId, "artifact.json");
  await fs.mkdir(path.dirname(sourcePath), { recursive: true });
  const request = setupRequest("snapshot_setup_12345678"); request.projects = [];
  let mode;
  const options = { octopusHomeRoot: home, registryRoot: registry, request, planOperatorSurface: async (input) => {
    const plan = await fixturePlan(home, registry, input);
    const source = { id: sourceId, content: pin(JSON.parse(sourceBytes)) };
    const sourceSha = crypto.createHash("sha256").update(sourceBytes).digest("hex");
    let wrapper = { id: snapshotId, run: plan.runId, source: structuredClone(source), source_sha256: sourceSha, source_content: sourceBytes.toString("utf8") };
    if (mode === "id") wrapper.id = "other_snapshot";
    if (mode === "run") wrapper.run = "other_run";
    if (mode === "source") wrapper.source.id = "other_source";
    if (mode === "source_sha256") wrapper.source_sha256 = "0".repeat(64);
    if (mode === "source_content") wrapper.source_content += " ";
    if (mode === "source_pin") { source.content[0] ^= 1; wrapper.source = structuredClone(source); }
    if (mode === "legacy_copy") wrapper = JSON.parse(sourceBytes);
    const snapshot = { id: snapshotId, content: pin(wrapper) };
    if (mode === "snapshot_pin") snapshot.content[0] ^= 1;
    if (mode === "current_source") await fs.appendFile(sourcePath, " ");
    const runWrite = plan.writes.find((write) => write.tag === `run:${plan.runId}`), run = JSON.parse(runWrite.content);
    run.interface_state[plan.modelInterfaceId] = { prelude_sources: [{ source, snapshot, source_sha256: sourceSha }] };
    runWrite.content = JSON.stringify(run);
    plan.writes.push(write(snapshotPath, JSON.stringify(wrapper, null, 2), `artifact:${snapshotId}`));
    if (mode === "legacy_copy") assert.deepEqual(source.content, snapshot.content);
    else assert.notDeepEqual(source.content, snapshot.content);
    return plan;
  } };
  for (mode of ["id", "run", "source", "source_sha256", "source_content", "source_pin", "snapshot_pin", "legacy_copy", "current_source"]) {
    await fs.writeFile(sourcePath, sourceBytes);
    await assert.rejects(initializeWorkspace(options), /Home prelude (snapshot|source)/);
    await assert.rejects(fs.stat(snapshotPath), /ENOENT/);
    assert.deepEqual(await fs.readdir(path.join(home, "state", "runs")), []);
  }
  mode = null; await fs.writeFile(sourcePath, sourceBytes);
  const result = await initializeWorkspace(options), saved = JSON.parse(await fs.readFile(snapshotPath));
  assert.equal(saved.id, snapshotId); assert.equal(saved.run, result.homeRunId);
  assert.deepEqual(Buffer.from(saved.source_content), sourceBytes);
  assert.deepEqual(await fs.readFile(sourcePath), sourceBytes);
  assert.equal((await initializeWorkspace(options)).changed, false);
  console.log("workspace prelude snapshot: wrapper identity, both content pins, original bytes, source drift and replay passed");
}

async function verifyLinkedTaskRemoval(temporary) {
  const home = path.join(temporary, "task-removal"), registry = path.join(home, "registry");
  const run = path.join(home, "state/runs/task.json"), owned = path.join(home, "interfaces/provider");
  const target = path.join(temporary, "shared-auth"), content = "shared account state\n";
  await createFixture(home, registry);
  await fs.mkdir(path.dirname(run), { recursive: true });
  await fs.mkdir(path.join(owned, "app-server-home"), { recursive: true });
  await fs.writeFile(run, "{}\n");
  await fs.writeFile(target, content);
  await fs.symlink(target, path.join(owned, "app-server-home/auth.json"));
  await fs.symlink(`${target}-missing`, path.join(owned, "dangling"));
  const stat = await fs.lstat(owned);
  const options = { octopusHomeRoot: home, registryRoot: registry, plan: {
    operationId: "task_remove_links_12345678", label: "delete task",
    writes: [{ destination: run, expectedBeforeDigest: crypto.createHash("sha256").update("{}\n").digest("hex"), content: null, tag: "run", publishBarrier: true }],
    removals: [{ destination: owned, type: "directory", expectedDev: stat.dev, expectedIno: stat.ino, tag: "private state" }],
  } };
  await commitClientAuthoringTransaction(options);
  await recoverWorkspaceTransactions(options);
  await commitClientAuthoringTransaction(options);
  await assert.rejects(fs.lstat(run), /ENOENT/);
  await assert.rejects(fs.lstat(owned), /ENOENT/);
  assert.equal(await fs.readFile(target, "utf8"), content);
  for (const mode of ["retired", "unpaired", "referenced", "replaced", "descriptor_changed", "metadata_alias"]) {
    const currentHome = path.join(temporary, `task-removal-${mode}`), currentRegistry = path.join(currentHome, "registry");
    await createFixture(currentHome, currentRegistry);
    const runPath = path.join(currentHome, "state/runs/task.json");
    const state = path.join(currentHome, "interfaces/provider");
    const descriptor = path.join(currentRegistry, "interface/provider");
    await fs.mkdir(state, { recursive: true }); await fs.mkdir(descriptor, { recursive: true });
    await fs.mkdir(path.dirname(runPath), { recursive: true });
    await fs.writeFile(runPath, "{}\n");
    await fs.writeFile(path.join(descriptor, "interface.json"), '{"id":"provider"}\n');
    await fs.writeFile(path.join(state, "ledger-cursor.json"), '{"seq":1}\n');
    await fs.symlink(target, path.join(state, "auth.json"));
    if (mode === "metadata_alias") await fs.symlink(target, path.join(state, ".octopus-publication"));
    if (mode === "referenced") await fs.writeFile(path.join(currentHome, "state/runs/other.json"), '{"interface_state":{"provider":{}}}\n');
    const removal = async (destination) => {
      const identity = await fs.lstat(destination);
      return { destination, type: "directory", expectedDev: identity.dev, expectedIno: identity.ino, tag: destination };
    };
    const plan = { ...options.plan, operationId: `task_remove_${mode}_12345678`,
      writes: [{ ...options.plan.writes[0], destination: runPath }],
      removals: [...(mode === "unpaired" ? [] : [await removal(descriptor)]), await removal(state)] };
    const unlink = fs.unlink;
    fs.unlink = async (candidate) => {
      await unlink(candidate);
      if (candidate !== runPath) return;
      if (mode === "replaced") {
        await fs.rename(state, `${state}-other`); await fs.mkdir(state);
      }
      if (mode === "descriptor_changed") await fs.writeFile(path.join(descriptor, "interface.json"), '{"id":"changed"}\n');
      await fs.writeFile(path.join(state, "ledger-cursor.json"), '{"seq":2}\n');
    };
    try {
      const commit = commitClientAuthoringTransaction({ octopusHomeRoot: currentHome, registryRoot: currentRegistry, plan });
      await assert.rejects(commit, /identity changed|still referenced|publication metadata/);
      assert.equal(await fs.readFile(target, "utf8"), content);
    } finally { fs.unlink = unlink; }
  }
}

async function verifyOperatorChatStatusProjection(temporary) {
  const fixtureHome = path.join(temporary, "operator-status-home");
  const fixtureRegistry = path.join(fixtureHome, "registry");
  await createFixture(fixtureHome, fixtureRegistry);
  const octopusHomeRoot = await fs.realpath(fixtureHome);
  const registryRoot = await fs.realpath(fixtureRegistry);
  const workspace = await initializeWorkspace({
    octopusHomeRoot,
    registryRoot,
    request: setupRequest("workspace_status_12345678"),
    planOperatorSurface: (input) => fixturePlan(octopusHomeRoot, registryRoot, input),
  });
  const protocolRoot = path.join(registryRoot, "interface", "world_operator_chat");
  await fs.mkdir(protocolRoot, { recursive: true });
  await fs.copyFile(
    path.join(__dirname, "../../registry/interface/world_operator_chat/client.json"),
    path.join(protocolRoot, "client.json"),
  );
  const ledgerRoot = path.join(octopusHomeRoot, "ledger");
  const ledgerPath = path.join(ledgerRoot, "movements.jsonl");
  await fs.mkdir(ledgerRoot, { recursive: true });
  await fs.writeFile(ledgerPath, "", "utf8");
  const ledgerIndexAvailable = createFixtureLedgerIndex(path.join(ledgerRoot, "ledger-index.sqlite"));
  const policyProjection = await fixturePolicyProjection(octopusHomeRoot);
  const store = createProjectionStore({
    resolveRepoRoot: () => path.resolve(__dirname, "../.."),
    sourceRegistry: () => registryRoot,
    octopusHome: () => octopusHomeRoot,
    projectEePolicies: async () => policyProjection,
  });
  const readStatuses = () => store.read({
    contract_version: 1,
    method: "projection.operator.chats.status",
    params: { run_ids: [workspace.homeRunId] },
  });
  try {
    const initial = await store.read({
      contract_version: 1,
      method: "projection.operator.chats.list",
    });
    const initialHomeChat = initial.chats.find((chat) => chat.run_id === workspace.homeRunId);
    assert.equal(initialHomeChat?.turn_state, "idle");
    assert.equal(
      initialHomeChat?.provider_interface_id,
      setupRequest("workspace_status_12345678").home.providerInterfaceId,
    );
    assert.equal(
      initialHomeChat?.provider_bound_interface_id,
      `${setupRequest("workspace_status_12345678").home.providerInterfaceId}_${workspace.homeRunId.replace("run_operator_chat_", "")}`,
    );
    const provider = initialHomeChat.provider_bound_interface_id;
    const workingMovements = [
      fixtureMovement(0, workspace, "operator.message", workspace.homeInterfaceId, "run"),
      fixtureMovement(1, workspace, "turn.started", `${setupRequest("workspace_status_12345678").home.providerInterfaceId}_${workspace.homeRunId.replace("run_operator_chat_", "")}`, "run"),
    ];
    for (const movement of workingMovements) await materializeFixtureMovement(octopusHomeRoot, movement);
    await fs.appendFile(ledgerPath, workingMovements.map((movement) => `${JSON.stringify(movement)}\n`).join(""));
    if (ledgerIndexAvailable) await indexFixtureMovements(ledgerRoot, workingMovements);
    const working = await readStatuses();
    assert.deepEqual(working.chats.map((chat) => ({
      run_id: chat.run_id,
      turn_open: chat.turn_open,
      turn_state: chat.turn_state,
      last_movement_seq: chat.last_movement_seq,
    })), [{
      run_id: workspace.homeRunId,
      turn_open: true,
      turn_state: "working",
      last_movement_seq: 1,
    }]);
    const firstReply = fixtureMovement(2, workspace, "interface.artifact.output", provider, "run");
    await materializeFixtureMovement(octopusHomeRoot, firstReply);
    await fs.appendFile(ledgerPath, `${JSON.stringify(firstReply)}\n`);
    const idle = await readStatuses();
    assert.equal(idle.chats[0].turn_open, false);
    assert.equal(idle.chats[0].turn_state, "idle");
    assert.equal(idle.chats[0].last_movement_seq, 2);
    assert.equal((await readStatuses()).ledger_cursor_seq, 2);
    const cases = [
      ["turn.started", "other-provider", {}, "idle"],
      ["provider.peer.input", "peer", { message: { action: "queue" } }, "idle"],
      ["provider.transport.dispatch", provider, { provider_interface: provider, method: "thread/injectItems" }, "idle"],
      ["provider.peer.input", "peer", { message: { action: "followup" } }, "waiting"],
      ["provider.transport.dispatch", provider, { provider_interface: provider, method: "turn/start" }, "working"],
      ["turn.reasoning", provider, { lifecycle: "completed" }, "working"],
      ["interface.artifact.output", "other-provider", {}, "working"],
      ["turn.error", provider, { will_retry: true }, "working"],
      [`${provider}.failure`, provider, { failure_class: "dispatch_failed", source_movement_seq: 6 }, "failed"],
      ["turn.started", provider, {}, "working"],
      ["turn.completed", provider, {}, "idle"],
      ["account.rate_limits", provider, {}, "idle"],
      ["provider.transport.dispatch", provider, { provider_interface: provider, method: "turn/start" }, "working"],
      ["interface.artifact.output", provider, {}, "idle"],
      ["turn.started", provider, {}, "working"],
      ["turn.error", provider, { will_retry: false }, "failed"],
      ["turn.started", provider, {}, "working"],
      ["turn.interrupted.result", provider, {}, "idle"],
      ["turn.started", provider, {}, "working"],
      ["turn.control.acknowledged", provider, { action: "interrupt", status: "acknowledged", control_id: "stop" }, "idle"],
      ["turn.started", provider, {}, "working"],
      ["turn.completed", provider, { status: "failed" }, "failed"],
      ["plain.note", provider, Buffer.from("plain text"), "failed"],
      ["binary.data", provider, Buffer.from([255, 0, 1]), "failed"],
      ["json.array", provider, [1, 2], "failed"],
      ["json.scalar", provider, 3, "failed"],
    ];
    await fs.mkdir(path.join(ledgerRoot, "blobs"), { recursive: true });
    for (const id of ["peer", "other-provider"]) {
      const directory = path.join(registryRoot, "interface", id);
      await fs.mkdir(directory, { recursive: true });
      await fs.writeFile(path.join(directory, "interface.json"), JSON.stringify({ id, interface_type: "world", role: "provider", direction: "bidirectional", source_names: ["fixture"] }));
    }
    for (const [offset, [label, from, payload, expected]] of cases.entries()) {
      const movement = fixtureMovement(offset + 3, workspace, label, from, "run");
      if (from === "peer" || label === `${provider}.failure`) {
        movement.kind = "InterfaceOutbound";
        movement.to = { Interface: from === "peer" ? provider : workspace.homeInterfaceId };
      }
      movement.recorded_at_unix_ms -= offset * 2;
      await materializeFixtureMovement(octopusHomeRoot, movement, payload);
      await fs.appendFile(ledgerPath, `${JSON.stringify(movement)}\n`);
      const status = (await readStatuses()).chats[0];
      assert.deepEqual([status.turn_state, status.turn_open], [expected, ["waiting", "working"].includes(expected)], label);
      store.invalidate();
      const cold = (await store.read({ contract_version: 1, method: "projection.operator.chats.list" })).chats.find((chat) => chat.run_id === workspace.homeRunId);
      assert.deepEqual([cold.turn_state, cold.turn_open], [status.turn_state, status.turn_open], `replay: ${label}`);
      if (["plain.note", "binary.data", "json.array", "json.scalar"].includes(label)) {
        const page = await store.read({ contract_version: 1, method: "projection.operator.run.activity", params: { run_id: workspace.homeRunId, after_seq: movement.seq - 1 } });
        assert.equal(page.activity.length, 1); assert.equal(page.activity[0].payload_error, "", label);
      }
    }
    let nextSeq = cases.length + 3;
    const appendStatus = async (label, from, payload) => {
      const movement = fixtureMovement(nextSeq++, workspace, label, from, "run");
      await materializeFixtureMovement(octopusHomeRoot, movement, payload);
      await fs.appendFile(ledgerPath, `${JSON.stringify(movement)}\n`);
      return (await readStatuses()).chats[0];
    };
    for (const mode of ["queued", "recorded", "failed", "retained", "retained-failed", "retained-other"]) {
      const recorded = mode === "recorded", queued = ["queued", "retained-other"].includes(mode), retained = mode.startsWith("retained");
      const first = nextSeq, latest = first + 1, thread = "thread", turn = `turn-${first}`, clientId = `input-${latest}`;
      await appendStatus("operator.message", workspace.homeInterfaceId, { message_id: `input-${first}` });
      await appendStatus("operator.message", workspace.homeInterfaceId, { message_id: clientId });
      await appendStatus("turn.started", provider, { thread_id: thread, turn_id: turn, source_movement_seq: first });
      const header = { type: "header", schema_version: 1, seq: 0, provider_interface: provider, run: workspace.homeRunId,
        thread_id: thread, turn_id: turn, opened_source_movement_seq: first, opened_at_unix_ms: 10 };
      const input = { type: "input_recorded", seq: 1, recorded_at_unix_ms: 11, source_movement_seq: first,
        provider_interface: provider, run: workspace.homeRunId, thread_id: thread, turn_id: turn, item_id: "native-input",
        client_id: recorded ? clientId : "unrelated", stage: "input_recorded_in_turn", source_event: "thread/resume" };
      const seal = { type: "seal", seq: 2, recorded_at_unix_ms: 12, source_movement_seq: first,
        terminal_label: "turn.completed", terminal_artifact_payload_blake3: "a".repeat(64) };
      await appendStatus("turn.stream", provider, Buffer.from([header, input, seal].map(JSON.stringify).join("\n") + "\n"));
      if (mode === "failed") assert.equal((await appendStatus("turn.control.failed", provider,
        { thread_id: thread, turn_id: turn, action: "steer", status: "failed", control_id: clientId, source_movement_seq: latest })).turn_open, true);
      const status = await appendStatus("turn.completed", provider, retained
        ? { threadId: thread, turn: { id: turn, status: mode === "retained-failed" ? "failed" : "interrupted", itemsView: "full",
          items: [{ type: "userMessage", id: "native-input", clientId: queued ? "other" : clientId, content: [] }] } }
        : { thread_id: thread, turn_id: turn });
      assert.deepEqual([status.turn_open, status.turn_state, status.turn_opened_movement_seq, status.last_movement_seq],
        [queued, queued ? "working" : mode === "retained-failed" ? "failed" : "idle", first, nextSeq - 1]);
      store.invalidate();
      assert.deepEqual((await readStatuses()).chats[0], status, "cold replay retains pending input and actual ledger frontier");
      if (queued) assert.equal((await appendStatus("turn.completed", provider,
        { thread_id: thread, turn_id: `${turn}-next`, source_movement_seq: latest })).turn_open, false);
      await appendStatus("operator.message", workspace.homeInterfaceId, { message_id: `input-${nextSeq}` });
      const delayed = await appendStatus("turn.completed", provider, { thread_id: thread, turn_id: turn });
      assert.equal(delayed.turn_open, true, "settled native identities exclude delayed old output");
      store.invalidate();
      assert.deepEqual((await readStatuses()).chats[0], delayed);
      assert.equal((await appendStatus("interface.artifact.output", provider, {})).turn_open, false);
    }
    const refusedSeq = nextSeq;
    await appendStatus("operator.message", workspace.homeInterfaceId, { message_id: "refused-input" });
    for (const [action, id, expected] of [["stop", "refused-input", true], ["steer", "other", true], ["steer", "refused-input", false]])
      assert.equal((await appendStatus("turn.control.failed", provider, { action, control_id: id, status: "failed", source_movement_seq: refusedSeq })).turn_open, expected);
    store.invalidate(); assert.equal((await readStatuses()).chats[0].turn_state, "failed");
    const retired = fixtureMovement(nextSeq, workspace, "turn.started", provider, "run");
    await materializeFixtureMovement(octopusHomeRoot, retired);
    await fs.appendFile(ledgerPath, `${JSON.stringify(retired)}\n`);
    assert.equal((await readStatuses()).chats[0].turn_state, "working");
    const runPath = path.join(octopusHomeRoot, "state", "runs", `${workspace.homeRunId}.json`);
    const record = JSON.parse(await fs.readFile(runPath, "utf8"));
    record.interface_state[provider] = { authoring_generation: 0, context_generation: 0,
      run_authoring_binding: { run: workspace.homeRunId } };
    await fs.writeFile(runPath, JSON.stringify(record));
    store.invalidate();
    assert.equal((await readStatuses()).chats[0].turn_open, false, "retired dispatch is history, not a turn in the fresh execution");
    record.interface_state[provider].authoring_generation = 1;
    await fs.writeFile(runPath, JSON.stringify(record));
    store.invalidate();
    assert.equal((await readStatuses()).chats[0].turn_open, false, "a control or catalog rearm cannot reopen the retired turn");
    const current = fixtureMovement(retired.seq + 1, workspace, "operator.message", workspace.homeInterfaceId, "run");
    await materializeFixtureMovement(octopusHomeRoot, current);
    current.invocation_metadata.runtime.checkpoint = "cp_home_input";
    await fs.appendFile(ledgerPath, `${JSON.stringify(current)}\n`);
    assert.equal((await readStatuses()).chats[0].turn_state, "waiting", "current input remains open before provider authoring");
    record.interface_state[provider].authoring_generation = 1;
    await fs.writeFile(runPath, JSON.stringify(record));
    store.invalidate();
    assert.equal((await readStatuses()).chats[0].turn_open, true, "normal rearming must not hide an unfinished turn");
    record.interface_state[provider].pending_handoff = { source_movement_seq: current.seq };
    record.checkpoints = record.checkpoints.filter((row) => row.id !== "cp_home_input");
    record.active_checkpoints = record.active_checkpoints.filter((id) => id !== "cp_home_input");
    await fs.writeFile(runPath, JSON.stringify(record));
    store.invalidate();
    assert.equal((await readStatuses()).chats[0].turn_open, true, "pending source retains a turn after its input checkpoint is replaced");
    const sidecarPath = path.join(registryRoot, "interface", workspace.homeInterfaceId, "octopus_os.json");
    const sidecar = JSON.parse(await fs.readFile(sidecarPath, "utf8"));
    sidecar.authoring_profile.external_inputs = [{ checkpoint_id: "cp_external_input", interface_id: "peer", artifact_names: ["sensor.observation"] }];
    await fs.writeFile(sidecarPath, JSON.stringify(sidecar));
    record.interface_state[workspace.homeInterfaceId].cycle_checkpoint_ids = [];
    delete record.interface_state[provider].pending_handoff;
    await fs.writeFile(runPath, JSON.stringify(record));
    store.invalidate();
    assert.equal((await readStatuses()).chats[0].turn_open, false);
    nextSeq = current.seq + 1;
    const external = fixtureMovement(nextSeq++, workspace, "sensor.observation", "peer", "run");
    external.kind = "InterfaceOutbound"; external.to = { Interface: provider };
    external.invocation_metadata.runtime.checkpoint = "cp_external_input";
    await materializeFixtureMovement(octopusHomeRoot, external, { text: "Review recent work." });
    await fs.appendFile(ledgerPath, `${JSON.stringify(external)}\n`);
    const waiting = (await readStatuses()).chats[0];
    assert.deepEqual([waiting.turn_open, waiting.turn_state, waiting.turn_opened_movement_seq], [true, "waiting", external.seq]);
    const running = await appendStatus("provider.transport.dispatch", provider, { provider_interface: provider, method: "turn/start" });
    assert.equal(running.turn_state, "working");
    store.invalidate();
    assert.deepEqual((await readStatuses()).chats[0], running, "external admission remains visible after its checkpoint is retired");
    assert.equal((await appendStatus("turn.completed", provider, {})).turn_open, false);
  } finally {
    store.close();
  }
}

async function verifyTaskArchive(temporary) {
  const fixtureHome = path.join(temporary, "task-archive-home");
  await createFixture(fixtureHome, path.join(fixtureHome, "registry"));
  const home = await fs.realpath(fixtureHome), registry = await fs.realpath(path.join(home, "registry"));
  const request = setupRequest("workspace_archive_12345678");
  for (const workerId of [request.home.workerInterfaceId, request.task.workerInterfaceId]) {
    const workerRoot = path.join(registry, "interface", workerId);
    await fs.mkdir(workerRoot, { recursive: true });
    await fs.writeFile(path.join(workerRoot, "interface.json"), JSON.stringify({ id: workerId, interface_type: "system", role: "worker", direction: "bidirectional", source_names: ["fixture"] }));
    await fs.writeFile(path.join(workerRoot, "policy.json"), JSON.stringify({ id: workerId, role_artifacts: [], ceiling: { interfaces: [], tools: [], wek: [], mark_outcomes: [], hard_blocked_tools: [] } }));
  }
  const workspace = await initializeWorkspace({ octopusHomeRoot: home, registryRoot: registry, request,
    planOperatorSurface: (input) => fixturePlan(home, registry, input) });
  const plan = await fixturePlan(home, registry, { eeInstancesRoot: path.join(home, "state", "ee_instances"),
    request: { ...request.home, eeId: workspace.projects[0].eeId, chatId: "archive_task_12345678", clientSurfaceRole: "task" } });
  for (const write of plan.writes) {
    await fs.mkdir(path.dirname(write.filePath), { recursive: true }); await fs.writeFile(write.filePath, write.content);
  }
  for (const [interfaceId, component] of [[plan.workerInterfaceId, "worker"], [plan.modelInterfaceId, "provider"]]) {
    await fs.writeFile(path.join(registry, "interface", interfaceId, "octopus_os.json"), JSON.stringify({
      schema_version: 1, workspace: "octopus_os", interface: interfaceId, owner_interface: plan.chatInterfaceId, component, surface_role: "task",
    }));
  }
  const protocolRoot = path.join(registry, "interface", "world_operator_chat");
  await fs.mkdir(protocolRoot, { recursive: true });
  await fs.copyFile(path.join(__dirname, "../../registry/interface/world_operator_chat/client.json"), path.join(protocolRoot, "client.json"));
  const runFile = path.join(home, "state", "runs", `${plan.runId}.json`), ledger = path.join(home, "ledger", "movements.jsonl");
  const original = JSON.parse(await fs.readFile(runFile, "utf8"));
  original.interface_state[plan.modelInterfaceId] = { authoring_generation: 1, pending_handoff: { terminal: true } };
  const intakeId = plan.requiredInterfaceIds.find((id) => id.startsWith("system_service_input_"));
  const doorId = plan.requiredInterfaceIds.find((id) => id.startsWith("system_input_writer_"));
  const toolId = plan.requiredToolIds.find((id) => id.startsWith("file.write.input_"));
  const intakePath = path.join(registry, "interface", intakeId, "interface.json");
  const providerPath = path.join(registry, "interface", plan.modelInterfaceId, "interface.json");
  const provider = JSON.parse(await fs.readFile(providerPath));
  provider.interface_driver_config.private_config.input_intake = { template_interface_id: "system_service_input", interface_id: intakeId };
  await fs.writeFile(providerPath, JSON.stringify(provider));
  const writer = { owner: intakeId, source: intakeId, door: doorId, tool: toolId, call_label: "tool.call", result_label: `${toolId}.result` };
  const declaration = { schema_version: 1, run: plan.runId, provider_interface: plan.modelInterfaceId, writer,
    writer_required_verified_artifacts: original.checkpoints[4].required_verified_artifacts,
    profile: { file_write_tool: toolId, file_write_label: writer.call_label, file_write_result_label: writer.result_label, result_timeout_ms: 30000 },
    provider_profile: { file_write_tool: "file.write.authoring", file_write_label: "tool.call", file_write_result_label: "file.write.authoring.result", result_timeout_ms: 30000 },
    input_label: "provider.peer.input", notification_action: "followup", artifact_bindings: [] };
  const intake = { id: intakeId, interface_type: "system", role: "service", direction: "bidirectional", source_names: ["input"],
    interface_driver_ref: "checked_input_intake", interface_driver_config: { private_config: { input_intake: declaration } } };
  await fs.writeFile(intakePath, JSON.stringify(intake));
  original.interface_state[intakeId] = { input_intake: declaration,
    owned_checkpoint_ids: ["cp_intake_input", "cp_intake_call", "cp_intake_result"], frontier_checkpoint_ids: ["cp_intake_call", "cp_intake_result"] };
  const writeRun = (run) => fs.writeFile(runFile, `${JSON.stringify(run, null, 2)}\n`);
  await writeRun(original); await fs.mkdir(path.dirname(ledger)); await fs.writeFile(ledger, "");
  let store = createProjectionStore({ resolveRepoRoot: () => path.resolve(__dirname, "../.."), sourceRegistry: () => registry,
    octopusHome: () => home, projectEePolicies: () => fixturePolicyProjection(home) });
  const read = (method, params) => store.read({ contract_version: 1, method, params });
  const digest = async () => crypto.createHash("sha256").update(await fs.readFile(runFile)).digest("hex");
  let operations = 0;
  const archiveIdentity = { home, workspace: { rootEeId: workspace.rootEeId, homeRunId: workspace.homeRunId, homeInterfaceId: workspace.homeInterfaceId } };
  const hooks = {
    withTaskAuthoringLock: async (id, operation) => { assert.equal(id, plan.runId); return operation(); },
    readTaskStatus: async (id) => { store.invalidate(false); return (await read("projection.operator.chats.status", { run_ids: [id] })).chats[0]; },
    commitClientAuthoring: (mutation) => commitClientAuthoringTransaction({ octopusHomeRoot: home, registryRoot: registry, plan: {
      operationId: `task_archive_contract_${operations++}`, label: mutation.label,
      writes: mutation.writes.map((write) => ({ destination: write.file, expectedBeforeDigest: write.beforeDigest,
        content: write.next === null ? null : `${JSON.stringify(write.next, null, 2)}\n`, tag: "task", publishBarrier: write.file === mutation.publishBarrierPath })),
      removals: (mutation.removals ?? []).map((entry) => ({ destination: entry.path, type: entry.type,
        expectedDev: entry.dev, expectedIno: entry.ino, tag: entry.label })),
    } }),
  };
  const mutate = async (archived, overrides = {}) => {
    store.invalidate(false);
    const scope = await read("projection.operator.os.scope");
    assert.equal(scope.status, "ready", JSON.stringify(scope.errors));
    return mutateOctopusOsWorkspace(registry, home, scope,
      { action: "set_task_archived", ...archiveIdentity, runId: plan.runId, expectedDigest: await digest(), archived, ...overrides }, hooks);
  };
  try {
    const peer = structuredClone(original), checkpoint = structuredClone(original.checkpoints[0]);
    checkpoint.id = "peer_home_result";
    checkpoint.interface_binding.interface = workspace.homeInterfaceId;
    peer.checkpoints.push(checkpoint); peer.active_checkpoints.push(checkpoint.id);
    peer.interface_state[workspace.homeInterfaceId] = {};
    await writeRun(peer); store.invalidate();
    const connected = await read("projection.operator.os.scope");
    assert.equal(connected.status, "ready", connected.errors.join("; "));
    assert.equal(connected.client_runs.find(row => row.run_id === plan.runId).interface_id, plan.chatInterfaceId);
    peer.interface_state[workspace.homeInterfaceId].client_surface_role = "home";
    await writeRun(peer); store.invalidate();
    assert.ok((await read("projection.operator.os.scope")).errors.some(error => error.includes("more than one exact")));
    await writeRun(original); store.invalidate();
    await assert.rejects(mutate(true, { runId: workspace.homeRunId }), /not an archivable/);
    await assert.rejects(mutate(true, { home: `${home}-other` }), /does not match the current Home/);
    await assert.rejects(mutate(true, { expectedDigest: "0".repeat(64) }), /changed before/);
    const commitsBefore = operations;
    for (const [change, error] of [
      [(run) => { run.interface_state.system_service_foreign = {}; }, /outside its exact client topology/],
      [(run) => { run.interface_state[intakeId].input_intake.run = "foreign_run"; }, /exact run\/provider declaration/],
      [(run) => { run.interface_state[intakeId].input_intake.writer.door = plan.modelInterfaceId; }, /exact run\/provider declaration/],
      [(run) => { run.interface_state[intakeId].owned_checkpoint_ids.push("cp_home_input"); }, /exact ownership/],
      [(run) => { run.interface_state[plan.modelInterfaceId].owned_checkpoint_ids = ["cp_intake_input"]; }, /exact ownership/],
    ]) {
      const invalid = structuredClone(original); change(invalid); await writeRun(invalid);
      const before = await fs.readFile(runFile, "utf8");
      await assert.rejects(mutate(true), error);
      assert.equal(await fs.readFile(runFile, "utf8"), before); assert.equal(operations, commitsBefore);
    }
    await writeRun(original);
    const foreignDescriptor = structuredClone(intake); foreignDescriptor.interface_driver_config.private_config.input_intake.run = "foreign_run";
    await fs.writeFile(intakePath, JSON.stringify(foreignDescriptor));
    await assert.rejects(mutate(true), /exact run\/provider declaration/);
    await fs.writeFile(intakePath, JSON.stringify(intake));
    const policyPath = path.join(registry, "interface", plan.workerInterfaceId, "policy.json");
    const policyBefore = await fs.readFile(policyPath, "utf8"), policy = JSON.parse(policyBefore);
    policy.ceiling.interfaces = policy.ceiling.interfaces.filter((id) => id !== intakeId);
    await fs.writeFile(policyPath, JSON.stringify(policy)); await assert.rejects(mutate(true), /exact worker ceiling/);
    await fs.writeFile(policyPath, policyBefore);
    assert.equal(operations, commitsBefore);
    const pending = structuredClone(original); pending.interface_state[plan.modelInterfaceId].pending_handoff.terminal = false;
    await writeRun(pending); await assert.rejects(mutate(true), /is working/); await writeRun(original);
    const taskIdentity = { ...workspace, homeRunId: plan.runId, homeInterfaceId: plan.chatInterfaceId };
    const taskInput = fixtureMovement(0, taskIdentity, "operator.message", plan.chatInterfaceId, "run");
    await materializeFixtureMovement(home, taskInput);
    await fs.appendFile(ledger, `${JSON.stringify(taskInput)}\n`);
    await assert.rejects(mutate(true), /open turn/);
    const reply = fixtureMovement(1, taskIdentity, "interface.artifact.output", plan.modelInterfaceId, "interface");
    reply.kind = "InterfaceOutbound";
    await materializeFixtureMovement(home, reply);
    await fs.appendFile(ledger, `${JSON.stringify(reply)}\n`);
    store.invalidate(false);
    const messagesBefore = await read("projection.operator.chat.messages", { run_id: plan.runId });
    const archived = await mutate(true); assert.equal(archived.changed, true);
    const saved = JSON.parse(await fs.readFile(runFile, "utf8"));
    assert.deepEqual(saved.active_checkpoints, []);
    assert.deepEqual(saved.interface_state[plan.chatInterfaceId].client_archive.active_checkpoints, original.active_checkpoints);
    assert.deepEqual(saved.checkpoints, original.checkpoints); assert.equal(saved.status, original.status);
    store.close(); store = createProjectionStore({ resolveRepoRoot: () => path.resolve(__dirname, "../.."), sourceRegistry: () => registry, octopusHome: () => home, projectEePolicies: () => fixturePolicyProjection(home) });
    const archivedChat = (await read("projection.operator.chats.list")).chats.find((chat) => chat.run_id === plan.runId);
    assert.equal(archivedChat.archived, true); assert.equal(archivedChat.writable, false); assert.equal(archivedChat.message_count, 2);
    assert.equal(archivedChat.provider_bound_interface_id, plan.modelInterfaceId);
    assert.equal((await read("projection.operator.chats.status", { run_ids: [plan.runId] })).chats[0].archived, true);
    assert.deepEqual(await read("projection.operator.chat.messages", { run_id: plan.runId }), messagesBefore);
    assert.equal((await mutate(true)).changed, false); assert.equal(operations, 1);
    const scope = await read("projection.operator.os.scope");
    await assert.rejects(mutateOctopusOsWorkspace(registry, home, scope,
      { action: "delete_task", runId: plan.runId, expectedDigest: await digest() }, hooks), /restore it before deleting/);
    const tampered = structuredClone(saved); tampered.checkpoints[0].epoch += 1; await writeRun(tampered);
    scope.client_runs.find((row) => row.run_id === plan.runId).record_digest = await digest();
    await assert.rejects(mutateOctopusOsWorkspace(registry, home, scope,
      { action: "set_task_archived", ...archiveIdentity, runId: plan.runId, expectedDigest: await digest(), archived: false }, hooks), /topology changed/);
    await writeRun(saved);
    const reopened = structuredClone(saved); reopened.active_checkpoints = [...original.active_checkpoints]; await writeRun(reopened);
    scope.client_runs.find((row) => row.run_id === plan.runId).record_digest = await digest();
    await assert.rejects(mutateOctopusOsWorkspace(registry, home, scope,
      { action: "set_task_archived", ...archiveIdentity, runId: plan.runId, expectedDigest: await digest(), archived: false }, hooks), /still admits artifacts/);
    await writeRun(saved);
    assert.equal((await mutate(false)).changed, true);
    assert.deepEqual(JSON.parse(await fs.readFile(runFile, "utf8")), original);
    store.invalidate(false);
    const restoredChat = (await read("projection.operator.chats.list")).chats.find((chat) => chat.run_id === plan.runId);
    assert.equal(restoredChat.archived, false); assert.equal(restoredChat.writable, true);
    assert.equal(operations, 2);
    console.log("task archive: checked writes, admission frontier, retained history, explicit restore and drift refusals passed");
    for (const interfaceId of [intakeId, doorId]) await fs.writeFile(path.join(registry, "interface", interfaceId, "octopus_os.json"), JSON.stringify({
      schema_version: 1, workspace: "octopus_os", interface: interfaceId, owner_interface: plan.chatInterfaceId, component: "worker_environment_door", surface_role: "task",
    }));
    const applicationId = "world_amendment_fixture", applicationRoot = path.join(registry, "interface", applicationId);
    await fs.mkdir(applicationRoot);
    await fs.writeFile(path.join(applicationRoot, "interface.json"), JSON.stringify({ id: applicationId,
      interface_type: "world", role: "sensor", direction: "inbound", source_names: ["fixture"] }));
    await fs.writeFile(path.join(applicationRoot, "application.json"), JSON.stringify({ schema_version: 3,
      id: applicationId, name: "Fixture", description: "Fixture", category: "connector", developer: "Fixture",
      version: "1", logo: "icon.svg", interfaces: [applicationId] }));
    store.invalidate(false);
    const amendmentScope = await read("projection.operator.os.scope");
    amendmentScope.environments.find((row) => row.ee_id === original.ee).formed_envelope.allowed_interfaces.push(applicationId);
    const amendmentWriterFile = path.join(home, "state", "runs", "amendment_writer.json");
    const genesis = { id: "amendment_writer", ee: original.ee, worker: original.worker, trace: "trace:amendment_writer",
      status: "idle", checkpoints: [], active_checkpoints: [], interface_state: {} };
    const genesisBytes = `${JSON.stringify(Object.fromEntries(Object.entries(genesis).sort(([a], [b]) => a.localeCompare(b))))}\n`, retainedRun = await fs.readFile(runFile, "utf8");
    await mutateOctopusOsWorkspace(registry, home, amendmentScope,
      { action: "configure_task_application", runId: plan.runId, expectedDigest: await digest(), applicationId, connected: true },
      { ...hooks, planTaskApplicationAmendment: async () => ({ removals: [], writes: [
        { filePath: amendmentWriterFile, expectedBeforeHash: null, content: genesisBytes },
        { filePath: runFile, expectedBeforeHash: await digest(), content: retainedRun },
      ] }), commitClientAuthoring: (request) => commitProjectionClientAuthoring(home, registry, request) });
    assert.equal(await fs.readFile(amendmentWriterFile, "utf8"), genesisBytes);
    assert.equal(await fs.readFile(runFile, "utf8"), retainedRun);
    assert.deepEqual(await read("projection.operator.chat.messages", { run_id: plan.runId }), messagesBefore);
    await fs.unlink(amendmentWriterFile); await fs.rm(applicationRoot, { recursive: true });
    const writerId = "declared_task_writer", writerIds = new Set(["cp_intake_call", "cp_intake_result"]);
    const writerCheckpoints = original.checkpoints.filter((cp) => writerIds.has(cp.id)).map((cp) => ({
      ...structuredClone(cp), run: writerId, trace: `trace:${writerId}:${cp.id}`,
    }));
    const initialWriter = { id: writerId, ee: original.ee, worker: original.worker, trace: `trace:${writerId}`, status: "idle",
      interface_state: { [intakeId]: { file_writer_target: { run: plan.runId, provider: intakeId, source: intakeId, door: doorId, generation: 0 } } },
      checkpoints: writerCheckpoints, active_checkpoints: [...writerIds].sort() };
    const writerFile = path.join(home, "state", "runs", `${writerId}.json`);
    const writerRun = structuredClone(initialWriter);
    writerRun.interface_state[intakeId].file_writer_target.generation = 2;
    writerRun.checkpoints.forEach((cp) => { cp.id = `${cp.id}_2`; cp.trace = `trace:${writerId}:${cp.id}`; });
    writerRun.active_checkpoints = writerRun.checkpoints.map((cp) => cp.id);
    await fs.writeFile(writerFile, JSON.stringify(writerRun));
    original.checkpoints = original.checkpoints.filter((cp) => !writerIds.has(cp.id));
    original.active_checkpoints = original.active_checkpoints.filter((id) => !writerIds.has(id));
    original.interface_state[intakeId].owned_checkpoint_ids = ["cp_intake_input"];
    original.interface_state[intakeId].frontier_checkpoint_ids = [];
    original.interface_state[intakeId].writer_run = { run: writerId, initial_record: initialWriter, initial_record_format: "compact", role_artifact_bindings: [] };
    await writeRun(original);
    const { blake3 } = require("@noble/hashes/blake3");
    const canonical = (value) => JSON.stringify(value, function (_key, entry) {
      return entry && !Array.isArray(entry) && typeof entry === "object"
        ? Object.fromEntries(Object.keys(entry).sort().map((key) => [key, entry[key]])) : entry;
    });
    const artifact = async (body) => {
      const file = path.join(registry, "artifact", body.id, "artifact.json");
      await fs.mkdir(path.dirname(file), { recursive: true }); await fs.writeFile(file, JSON.stringify(body));
      return { id: body.id, content: [...blake3(Buffer.from(canonical(body)))] };
    };
    const shared = { id: "shared_source", schema_version: 1, module_source: "source bytes" };
    const source = await artifact(shared), sourceContent = JSON.stringify(shared);
    const sourceSha = crypto.createHash("sha256").update(sourceContent).digest("hex");
    const snapshot = await artifact({ id: "retained_source", run: plan.runId, source, source_sha256: sourceSha, source_content: sourceContent });
    const preludeBody = { id: "private_prelude", run: plan.runId, owner_interface: doorId, schema_version: 1, module_source: "compiled", exports: [] };
    const prelude = await artifact(preludeBody);
    const topology = await artifact({ id: "private_topology", run: plan.runId, worker: plan.workerInterfaceId, owner_interface: doorId, prelude_artifact: prelude });
    await fs.writeFile(path.join(registry, "interface", doorId, "code_mode.json"), JSON.stringify({
      context: { mode: "persistent", prelude_artifact: prelude }, handoff: { topology_artifact: topology },
    }));
    original.interface_state[doorId] = { prelude_sources: [{ source, snapshot, source_sha256: sourceSha }] };
    await writeRun(original);
    const unrelatedFile = path.join(registry, "artifact", "private_prelude_unrelated", "artifact.json");
    await artifact({ id: "private_prelude_unrelated", text: "unrelated" });
    const payloadFile = path.join(registry, "artifact", prelude.id, "retained.payload");
    await fs.writeFile(payloadFile, "retained payload evidence");
    const blobFile = path.join(home, "ledger", "retained.blob"); await fs.writeFile(blobFile, "retained blob evidence");
    const ledgerBefore = await fs.readFile(ledger, "utf8");
    for (const interfaceId of [intakeId, doorId]) {
      await fs.unlink(path.join(registry, "interface", interfaceId, "octopus_os.json"));
    }
    const remove = async (extraHooks = {}) => {
      store.invalidate(false);
      return mutateOctopusOsWorkspace(registry, home, await read("projection.operator.os.scope"),
        { action: "delete_task", runId: plan.runId, expectedDigest: await digest() }, { ...hooks, ...extraHooks });
    };
    const writerEnvironmentId = `we_${"b".repeat(32)}`;
    const formationFile = path.join(home, "interfaces", doorId, "worker-environments", writerEnvironmentId, "worker_environment.json");
    await fs.mkdir(path.dirname(formationFile), { recursive: true });
    const writerFormation = { schema_version: 5, lifecycle: "active", formation: {
      worker_environment_id: writerEnvironmentId, run: writerId, ee: original.ee,
      worker: original.worker, door_interface: doorId, requires_release: false,
    } };
    for (const invalid of [{ run: "another_run" }, { requires_release: true }]) {
      await fs.writeFile(formationFile, JSON.stringify({ ...writerFormation, formation: { ...writerFormation.formation, ...invalid } }));
      await assert.rejects(remove(), /does not belong to task|cannot be deleted before/);
    }
    await fs.writeFile(formationFile, JSON.stringify(writerFormation));
    const sharedPath = path.join(registry, "artifact", shared.id, "artifact.json");
    await fs.writeFile(sharedPath, JSON.stringify({ ...shared, reference: prelude.id }));
    await assert.rejects(remove(), /task topology is still referenced/); await artifact(shared);
    const otherRun = path.join(home, "state", "runs", "another_run.json");
    await fs.writeFile(otherRun, JSON.stringify({ id: "another_run", reference: writerId }));
    await assert.rejects(remove(), /referenced by run/);
    await fs.writeFile(otherRun, JSON.stringify({ id: "another_run", context: { id: snapshot.id } }));
    await assert.rejects(remove(), /referenced by run/); await fs.unlink(otherRun);
    await fs.writeFile(writerFile, JSON.stringify({ ...writerRun, ee: "unrelated_ee" }));
    await assert.rejects(remove(), /drifted from its declared generation/); await fs.writeFile(writerFile, JSON.stringify(writerRun));
    await artifact({ ...preludeBody, run: "another_run" });
    await assert.rejects(remove(), /drifted identity, ownership or content/); await artifact(preludeBody);
    const preludePath = path.join(registry, "artifact", prelude.id, "artifact.json");
    await assert.rejects(remove({ releaseTaskWorkerEnvironments: async () => {
      await fs.writeFile(preludePath, JSON.stringify({ ...preludeBody, module_source: "changed" }));
      return { runId: plan.runId, expectedDigest: await digest(), releasedWorkerEnvironmentIds: [] };
    } }), /drifted identity, ownership or content/); await artifact(preludeBody);
    assert.equal(operations, 2);
    const deleted = await remove();
    assert.deepEqual(deleted.removedRuns, [plan.runId, writerId].sort());
    await assert.rejects(fs.stat(writerFile), /ENOENT/);
    assert.deepEqual(deleted.removedArtifacts, [prelude.id, topology.id, snapshot.id].sort());
    for (const id of deleted.removedArtifacts) await assert.rejects(fs.stat(path.join(registry, "artifact", id, "artifact.json")), /ENOENT/);
    await assert.rejects(fs.stat(runFile), /ENOENT/);
    assert.equal(await fs.readFile(sharedPath, "utf8"), sourceContent);
    assert.equal(JSON.parse(await fs.readFile(unrelatedFile)).text, "unrelated");
    assert.equal(await fs.readFile(payloadFile, "utf8"), "retained payload evidence");
    assert.equal(await fs.readFile(blobFile, "utf8"), "retained blob evidence");
    assert.equal(await fs.readFile(ledger, "utf8"), ledgerBefore);
    console.log("task deletion: exact artifact ownership, shared references, drift and retained evidence passed");
  } finally { store.close(); }
}

function createFixtureLedgerIndex(indexPath) {
  let DatabaseSync;
  try {
    ({ DatabaseSync } = require("node:sqlite"));
  } catch (error) {
    if (error?.code === "ERR_UNKNOWN_BUILTIN_MODULE") return false;
    throw error;
  }
  const database = new DatabaseSync(indexPath);
  try {
    database.exec(`
      CREATE TABLE index_meta(key TEXT PRIMARY KEY, value TEXT NOT NULL);
      CREATE TABLE movements(
        seq INTEGER PRIMARY KEY,
        lamport INTEGER NOT NULL,
        recorded_at_unix_ms INTEGER NOT NULL,
        trace TEXT NOT NULL,
        kind TEXT NOT NULL,
        mark TEXT,
        from_kind TEXT NOT NULL,
        from_id TEXT NOT NULL,
        to_kind TEXT NOT NULL,
        to_id TEXT NOT NULL,
        artifact_id TEXT,
        artifact_label TEXT,
        artifact_content TEXT,
        runtime_run TEXT,
        runtime_checkpoint TEXT,
        runtime_interface TEXT,
        start_offset INTEGER,
        end_offset INTEGER
      );
      INSERT INTO index_meta(key, value) VALUES
        ('schema_version', '4'),
        ('watermark', '0'),
        ('durable_end_offset', '0');
    `);
  } finally {
    database.close();
  }
  return true;
}

async function indexFixtureMovements(ledgerRoot, movements) {
  const { DatabaseSync } = require("node:sqlite");
  const database = new DatabaseSync(path.join(ledgerRoot, "ledger-index.sqlite"));
  try {
    const insert = database.prepare(`INSERT INTO movements(
      seq, lamport, recorded_at_unix_ms, trace, kind,
      from_kind, from_id, to_kind, to_id,
      artifact_id, artifact_label, artifact_content,
      runtime_run, runtime_checkpoint, runtime_interface
    ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`);
    for (const movement of movements) {
      const [fromKind, fromId] = Object.entries(movement.from)[0];
      const [toKind, toId] = Object.entries(movement.to)[0];
      insert.run(
        movement.seq,
        movement.lamport,
        movement.recorded_at_unix_ms,
        movement.trace,
        movement.kind,
        fromKind.toLowerCase(),
        fromId,
        toKind.toLowerCase(),
        toId,
        movement.artifact.id,
        movement.invocation_metadata.checkpoint_admission.artifact_label,
        Buffer.from(movement.artifact.content).toString("hex"),
        movement.invocation_metadata.runtime.run,
        movement.invocation_metadata.runtime.checkpoint,
        movement.invocation_metadata.runtime.interface,
      );
    }
    const ledgerSize = (await fs.stat(path.join(ledgerRoot, "movements.jsonl"))).size;
    database.prepare("UPDATE index_meta SET value = ? WHERE key = 'watermark'").run(String(movements.length));
    database.prepare("UPDATE index_meta SET value = ? WHERE key = 'durable_end_offset'").run(String(ledgerSize));
  } finally {
    database.close();
  }
}

async function fixturePolicyProjection(octopusHomeRoot) {
  const policy = {
    allowed_interfaces: [],
    allowed_tools: [],
    allowed_worker_environment_kinds: [],
    allowed_root_slots: [],
    allowed_artifact_verbs: [],
    hard_blocked_tools: [],
  };
  const files = (await fs.readdir(path.join(octopusHomeRoot, "state", "ee_instances")))
    .filter((name) => name.endsWith(".json"))
    .sort();
  const environments = [];
  for (const file of files) {
    const record = JSON.parse(await fs.readFile(path.join(octopusHomeRoot, "state", "ee_instances", file), "utf8"));
    environments.push({
      ee_id: record.ee_id,
      effective_policy: policy,
      policy_ceiling: policy,
      workers: [],
    });
  }
  return { environments, issues: [] };
}

async function materializeFixtureMovement(home, movement, payload = {}) {
  const root = path.join(home, "ledger", "blobs"), blake3 = require("@noble/hashes/blake3").blake3;
  await fs.mkdir(root, { recursive: true });
  const payloadBytes = Buffer.isBuffer(payload) ? payload : Buffer.from(JSON.stringify(payload)), payloadHash = Buffer.from(blake3(payloadBytes)).toString("hex");
  const label = movement.invocation_metadata.checkpoint_admission.artifact_label;
  const manifest = Buffer.from(JSON.stringify({ label, payload_path: path.join(root, payloadHash), payload_content_hash: payloadHash }));
  const hash = Buffer.from(blake3(manifest));
  await fs.writeFile(path.join(root, payloadHash), payloadBytes);
  await fs.writeFile(path.join(root, hash.toString("hex")), manifest);
  movement.artifact = { id: `${label}:${hash.toString("hex")}`, content: [...hash] };
}

function fixtureMovement(seq, workspace, label, fromInterface, toKind) {
  const run = workspace.homeRunId;
  return {
    seq,
    lamport: seq + 1,
    recorded_at_unix_ms: 1_780_000_000_000 + seq,
    trace: `trace:${run}:status:${seq}`,
    kind: "InterfaceArrival",
    from: { Interface: fromInterface },
    to: { Interface: toKind === "run" ? `system_worker_status_${run.replace("run_operator_chat_", "")}` : workspace.homeInterfaceId },
    artifact: { id: `${label}:${String(seq).padStart(64, "0")}`, content: Array(32).fill(seq) },
    invocation_metadata: {
      checkpoint_admission: { artifact_label: label, complete: true, required_artifact_labels: [] },
      runtime: {
        checkpoint: `cp_status_${seq}`,
        ee: workspace.rootEeId,
        epoch: 0,
        interface: fromInterface,
        run,
        worker: `system_worker_status_${run.replace("run_operator_chat_", "")}`,
      },
    },
  };
}

async function verifyLiveWorkspaceLock(temporary) {
  for (const signal of ["SIGKILL", "SIGCONT"]) {
    const home = path.join(temporary, `workspace-held-${signal}`), registry = path.join(home, "registry");
    await createFixture(home, registry);
    const child = childProcess.spawn(process.execPath, [__filename, "hold-workspace-child", home, registry], { stdio: ["ignore", "pipe", "pipe"] });
    const { once } = require("node:events"), exited = once(child, "exit");
    let stderr = ""; child.stderr.on("data", chunk => { stderr += chunk; });
    try {
      assert.equal(await Promise.race([once(child.stdout, "data").then(([data]) => data.toString()), exited.then(() => { throw new Error(`Workspace writer exited before its transaction: ${stderr}`); })]), "paused\n");
      const transactions = path.join(home, "client-state/workspace/transactions");
      const journal = path.join(transactions, "workspace_crash_12345678/journal.json");
      const before = await fs.readFile(journal);
      let settled = false;
      const read = readWorkspaceSetupState({ octopusHomeRoot: home, registryRoot: registry }).finally(() => { settled = true; });
      await new Promise(resolve => setTimeout(resolve, 100));
      assert.equal(settled, false, "a reader must wait for the live writer instead of recovering it");
      assert.deepEqual(await fs.readFile(journal), before);
      child.kill(signal); await exited;
      const state = await read;
      assert.equal(state.status, "configured");
      assert.equal(state.receipt.operation_id, "workspace_crash_12345678");
      assert.deepEqual(await fs.readdir(transactions), []);
    } finally {
      if (child.exitCode === null && child.signalCode === null) { child.kill("SIGKILL"); await exited; }
    }
  }
}

async function crashChild(octopusHomeRoot, registryRoot, hold = false) {
  let paused = false;
  await initializeWorkspace({
    octopusHomeRoot,
    registryRoot,
    request: setupRequest("workspace_crash_12345678"),
    planOperatorSurface: (input) => fixturePlan(octopusHomeRoot, registryRoot, input),
    onProgress: (event) => {
      if (event.phase !== "applying") return;
      if (!hold) process.exit(73);
      if (!paused) { paused = true; require("node:fs").writeSync(1, "paused\n"); process.kill(process.pid, "SIGSTOP"); }
    },
  });
  if (!hold) process.exit(74);
}

async function projectCrashChild(
  mode,
  octopusHomeRoot,
  registryRoot,
  rootEeId,
  homeRunId,
  homeInterfaceId,
  targetEeId,
) {
  const workspace = { rootEeId, homeRunId, homeInterfaceId };
  const scope = await fixtureProjectScope(octopusHomeRoot, registryRoot, workspace);
  if (mode === "create") {
    const operationId = "project_crash_create_12345678";
    armProjectCrash(
      octopusHomeRoot,
      operationId,
      path.join(registryRoot, "interface", homeInterfaceId, "octopus_os.json"),
    );
    await mutateWorkspaceProject({
      octopusHomeRoot,
      registryRoot,
      scope,
      planOperatorSurface: (input) => fixturePlan(octopusHomeRoot, registryRoot, input),
      request: {
        action: "create_project",
        operationId,
        workspace: fixtureWorkspaceIdentity(scope),
        parentEeId: rootEeId,
        expectedParentDigest: scope.environments.find((environment) => environment.eeId === rootEeId).recordDigest,
        templateName: "project",
        name: "Crash Recovery",
        goal: "Crash Recovery",
        presentation: { icon: "bolt", color: "amber" },
      },
    });
  } else if (mode === "delete") {
    const operationId = "project_crash_delete_12345678";
    const target = scope.environments.find((environment) => environment.eeId === targetEeId);
    assert.ok(target);
    armProjectCrash(
      octopusHomeRoot,
      operationId,
      path.join(octopusHomeRoot, "state", "ee_instances", `${targetEeId}.json`),
    );
    await mutateWorkspaceProject({
      octopusHomeRoot,
      registryRoot,
      scope,
      request: {
        action: "delete_project",
        operationId,
        workspace: fixtureWorkspaceIdentity(scope),
        eeId: targetEeId,
        expectedProjectDigest: target.recordDigest,
      },
    });
  } else {
    throw new Error(`unknown Project crash mode '${mode}'`);
  }
  process.exit(74);
}

function armProjectCrash(octopusHomeRoot, operationId, publishBarrierPath) {
  const journalPath = path.join(
    octopusHomeRoot,
    "client-state",
    "workspace",
    "transactions",
    operationId,
    "journal.json",
  );
  const originalRename = fs.rename;
  const originalOpen = fs.open;
  const originalUnlink = fs.unlink;
  let armed = false;
  fs.rename = async (source, destination) => {
    if (destination === journalPath) {
      const journal = JSON.parse(await fs.readFile(source, "utf8"));
      armed = journal.schema === "octopus.workspace_project_transaction"
        && journal.phase === "committing"
        && journal.writes[0]?.state === "applied"
        && journal.writes.some((writeRow) => writeRow.publish_barrier && writeRow.state === "staged");
    }
    return originalRename(source, destination);
  };
  fs.open = async (file, ...args) => {
    if (armed && file === publishBarrierPath) process.exit(73);
    return originalOpen(file, ...args);
  };
  fs.unlink = async (file) => {
    if (armed && file === publishBarrierPath) process.exit(73);
    return originalUnlink(file);
  };
}

async function verifyProjectionReadinessBarrier(temporary) {
  const fixtureHome = path.join(temporary, "projection-readiness-home");
  const registryRoot = path.join(fixtureHome, "registry");
  await createFixture(fixtureHome, registryRoot);
  const octopusHomeRoot = await fs.realpath(fixtureHome);
  const runsRoot = path.join(octopusHomeRoot, "state", "runs");
  await fs.mkdir(runsRoot, { recursive: true });
  await fs.writeFile(path.join(octopusHomeRoot, "host.json"), JSON.stringify({
    host_manifest_version: 1,
    root_refs: { run_records_root: runsRoot },
  }));
  let mode = "pass";
  let releaseRecovery;
  let beforeReadCalls = 0;
  let workspaceReadCalls = 0;
  let policyProjectionCalls = 0;
  const recovery = new Promise((resolve) => { releaseRecovery = resolve; });
  let store;
  await readWorkspaceSetupState({ octopusHomeRoot, registryRoot });
  store = createProjectionStore({
    resolveRepoRoot: () => path.resolve(__dirname, "../.."),
    sourceRegistry: () => registryRoot,
    octopusHome: () => octopusHomeRoot,
    inspectWorkspaceSetupState: async options => {
      workspaceReadCalls += 1;
      assert.equal(options.octopusHomeRoot, octopusHomeRoot);
      assert.equal(options.registryRoot, registryRoot);
      return inspectWorkspaceSetupState(options);
    },
    projectEePolicies: async () => {
      policyProjectionCalls += 1;
      return { environments: [], issues: [] };
    },
    beforeRead: async () => {
      beforeReadCalls += 1;
      if (mode === "defer") await recovery;
      if (mode === "reject") throw new Error("workspace recovery failed closed");
      if (mode === "defer" || mode === "retry") store.invalidate();
    },
  });
  const request = { contract_version: 1, method: "projection.ee_instances.list", params: { include_ledger: false } };
  try {
    const initial = await store.read(request);
    assert.ok(workspaceReadCalls > 0, "projection must use the injected workspace inspection owner");
    assert.equal(initial.ee_instances.some((entry) => entry.ee_id === "ee_after_recovery"), false);
    assert.equal(policyProjectionCalls, 1);
    store.invalidate(false);
    await store.read(request);
    assert.equal(policyProjectionCalls, 1);
    mode = "defer";
    let settled = false;
    const deferred = store.read(request);
    deferred.then(() => { settled = true; }, () => { settled = true; });
    await new Promise((resolve) => setImmediate(resolve));
    assert.equal(settled, false);
    await fs.writeFile(path.join(octopusHomeRoot, "state", "ee_instances", "ee_after_recovery.json"), `${JSON.stringify({
      ee_id: "ee_after_recovery",
      template_name: "project",
      parent_ee_id: "ee_existence",
      goal: "Recovered before projection",
      config: {},
    }, null, 2)}\n`);
    releaseRecovery();
    const recovered = await deferred;
    assert.equal(recovered.ee_instances.some((entry) => entry.ee_id === "ee_after_recovery"), true);
    mode = "reject";
    await assert.rejects(store.read(request), /workspace recovery failed closed/);
    await fs.writeFile(path.join(octopusHomeRoot, "state", "ee_instances", "ee_after_retry.json"), `${JSON.stringify({
      ee_id: "ee_after_retry",
      template_name: "project",
      parent_ee_id: "ee_existence",
      goal: "Recovered on retry",
      config: {},
    }, null, 2)}\n`);
    mode = "retry";
    const retried = await store.read(request);
    assert.equal(retried.ee_instances.some((entry) => entry.ee_id === "ee_after_retry"), true);
    assert.equal(beforeReadCalls, 5);
  } finally {
    store.close();
  }
}

async function verifyCapabilityPreferenceSetup() {
  const root = await fs.realpath(await fs.mkdtemp(path.join(os.tmpdir(), "octopus-capability-preferences-")));
  const browser = require("../../registry/worker_environment_kind/browser_session/packages/octopus-browser-session-env/native/setup-host.cjs");
  const desktop = require("../../registry/worker_environment_kind/desktop_session/packages/octopus-system-gui-env/native/setup-preferences.cjs");
  const userHome = process.env.HOME;
  try {
    process.env.HOME = root;
    await fs.mkdir(path.join(root, "Library", "Application Support"), { recursive: true });
    for (const fail of [false, true]) {
      const home = path.join(root, fail ? "rollback" : "complete"), registry = path.join(home, "registry");
      await createFixture(home, registry);
      const original = `${JSON.stringify({ host_manifest_version:1,root_refs:{ retained_root:root } })}\n`;
      await fs.writeFile(path.join(home,"host.json"),original);
      const request = setupRequest(fail ? "prefs_rollback_12345678" : "prefs_complete_12345678"); request.projects = [];
      const rollback = [];
      const options = { octopusHomeRoot:home, registryRoot:registry, request,
        planOperatorSurface:async input => {
          const plan = await fixturePlan(home,registry,input);
          for (const family of ["browser", "gui"]) {
            const id = `system_${family}_${input.request.chatId}`, dir=path.join(registry,"interface",id);
            plan.requiredInterfaceIds.push(id);
            plan.writes.push(write(path.join(dir,"interface.json"),JSON.stringify({id,...(family==='browser'?{interface_driver_config:{private_config:{browser_room:{transport:{wire:'browser.native.v1',host_root_slot:'browser_native_host_root'}}}}}:{})}),`interface:${id}`));
            if (family==='gui') plan.writes.push(write(path.join(dir,"desktop.json"),JSON.stringify({host:{application_preferences:{root_slot:'desktop_app_preferences_root'}}}),`desktop:${id}`));
            const runWrite = plan.writes.find(row => row.tag.startsWith('run:')), run = JSON.parse(runWrite.content);
            run.checkpoints[0].outbound_interface_bindings.push({interface:id,config:{artifact_policy:{mode:'all'}}});
            runWrite.content = JSON.stringify(run);
            const policyWrite = plan.writes.find(row => row.tag.startsWith('policy:')), policy = JSON.parse(policyWrite.content);
            policy.ceiling.interfaces.push(id); policyWrite.content = JSON.stringify(policy);
          }
          plan.requiredInterfaceIds.sort();return plan;
        },
        planBrowserNativeHost:async input=>{const plan=await browser.planBrowserNativeHost(input);return {...plan,rollback:async()=>{rollback.push('browser');await plan.rollback();}};},
        planDesktopApplicationPreferences:async input=>{
          assert.equal(input.plannedWrites.filter(row=>row.destination===path.join(home,'host.json')).length,1);
          const nullPreferences={destination:path.join(registry,'interface','system_gui_disabled','desktop.json'),content:JSON.stringify({host:{application_preferences:null}})};
          const plan=await desktop.planDesktopApplicationPreferences({...input,plannedWrites:[nullPreferences,...input.plannedWrites]});
          const host=JSON.parse(plan.writes[0].content);assert.equal(host.root_refs.browser_native_host_root,path.join(home,'client/browser-native-host'));
          assert.equal(host.root_refs.desktop_app_preferences_root,path.join(home,'client/desktop-app-preferences'));assert.equal(host.root_refs.retained_root,root);
          assert.equal(host.root_refs.desktop_device_lock_root,path.join(root,'Library/Application Support/Octopus/desktop-device'));
          return {...plan,writes:plan.writes.map(row=>({...row,expectedBeforeDigest:fail?'0'.repeat(64):row.expectedBeforeDigest})),rollback:async()=>{rollback.push('desktop');await plan.rollback();}};
        },
      };
      if(fail){
        await assert.rejects(initializeWorkspace(options),/invalid merged host manifest plan/);
        assert.deepEqual(rollback,['desktop','browser']);assert.equal(await fs.readFile(path.join(home,'host.json'),'utf8'),original);
        for (const name of ['browser-native-host','desktop-app-preferences']) await assert.rejects(fs.access(path.join(home,'client',name)), {code:'ENOENT'});
        assert.equal((await fs.stat(path.join(home,'client','agents'))).isDirectory(),true,'shared Agent setup is preserved');
      }else{
        const outcome=await initializeWorkspace(options);assert.equal((await readWorkspaceSetupState(options)).status,'configured');
        const receipt=JSON.parse(await fs.readFile(path.join(home,'client-state/workspace/receipts',`${request.operationId}.json`),'utf8'));
        assert.equal(receipt.writes.filter(row=>row.path===path.join(home,'host.json')).length,1);
        assert.equal(JSON.parse(await fs.readFile(path.join(home,'host.json'),'utf8')).root_refs.retained_root,root);assert.deepEqual(rollback,[]);
        assert.ok(outcome.homeRunId);
      }
    }
    console.log('Capability preference setup: actual two helpers merge one host write, preserve original roots, and roll back both lifetimes before publication');
  } finally {process.env.HOME = userHome; await fs.rm(root,{recursive:true,force:true});}
}

async function verifyRetiredConsentSetup() {
  const temporary = await fs.mkdtemp(path.join(os.tmpdir(), "octopus-consent-setup-test-"));
  let store;
  try {
    const home = path.join(await fs.realpath(temporary), "home");
    const registry = path.join(home, "registry");
    await createFixture(home, registry);
    const request = setupRequest("consent_setup_12345678");
    request.projects = [];
    const workerId = request.task.workerInterfaceId, workerRoot = path.join(registry, "interface", workerId);
    await fs.mkdir(workerRoot);
    await fs.writeFile(path.join(workerRoot, "interface.json"), JSON.stringify({ id: workerId, interface_type: "system", role: "worker", direction: "bidirectional", source_names: ["fixture"] }));
    await fs.writeFile(path.join(workerRoot, "policy.json"), JSON.stringify({ id: workerId, role_artifacts: [], ceiling: { interfaces: [], tools: [], wek: [], mark_outcomes: [], hard_blocked_tools: [] } }));
    let plannerCalls = 0;
    const options = { octopusHomeRoot: home, registryRoot: registry, request,
      planOperatorSurface: async (input) => { plannerCalls += 1; return fixturePlan(home, registry, input); } };
    await assert.rejects(initializeWorkspace({ ...options, request: { ...request,
      applicationConsentAuthority: { packId: "codex", expectedPackManifestDigest: "1".repeat(64) } } }), /unknown fields: applicationConsentAuthority/);
    assert.equal(plannerCalls, 0);
    await assert.rejects(fs.access(path.join(home, "state", "runs")), { code: "ENOENT" });
    const outcome = await initializeWorkspace(options);
    assert.equal(plannerCalls, 2, "Home and root Task surfaces both require preflight");
    const descriptorFile = path.join(registry, "interface", outcome.homeInterfaceId, "interface.json");
    const descriptor = JSON.parse(await fs.readFile(descriptorFile, "utf8"));
    assert.equal(descriptor.interface_driver_config?.private_config?.application_consent_authority, undefined);
    assert.equal((await readWorkspaceSetupState(options)).status, "configured");
    await fs.mkdir(path.join(home, "ledger")); await fs.writeFile(path.join(home, "ledger", "movements.jsonl"), "");
    store = createProjectionStore({ resolveRepoRoot: () => path.resolve(__dirname, "../.."), sourceRegistry: () => registry, octopusHome: () => home, projectEePolicies: () => fixturePolicyProjection(home) });
    const readScope = () => store.read({ contract_version: 1, method: "projection.operator.os.scope" });
    const initialScope = await readScope();
    assert.equal(initialScope.status, "ready", JSON.stringify(initialScope));
    const rootFile = path.join(home, "state", "ee_instances", `${outcome.rootEeId}.json`);
    const child = { ...JSON.parse(await fs.readFile(rootFile, "utf8")), ee_id: "ee_model_created", parent_ee_id: outcome.rootEeId, goal: "Model-authored goal" };
    await fs.writeFile(path.join(home, "state", "ee_instances", `${child.ee_id}.json`), JSON.stringify(child));
    const sibling = { ...child, ee_id: "ee_independent_service", parent_ee_id: null, goal: "Maintain client" };
    await fs.writeFile(path.join(home, "state", "ee_instances", `${sibling.ee_id}.json`), JSON.stringify(sibling));
    store.invalidate();
    const expanded = await readScope();
    assert.equal(expanded.status, "ready", expanded.errors.join("; "));
    assert.ok(expanded.projects.some((project) => project.ee_id === child.ee_id));
    assert.ok(!expanded.projects.some((project) => project.ee_id === sibling.ee_id));
    assert.ok(!expanded.environments.some((environment) => environment.ee_id === sibling.ee_id));
    assert.ok(!expanded.task_authoring_profiles.some((profile) => profile.ee_id === child.ee_id));
    assert.ok(!expanded.project_presentations.some((project) => project.ee_id === child.ee_id));
    const selected = { ...descriptor, interface_driver_config: { ...descriptor.interface_driver_config,
      private_config: { ...descriptor.interface_driver_config?.private_config,
        application_consent_authority: { id: "artifact-retired-consent", content: Array(32).fill(1) } } } };
    const selectedBytes = `${JSON.stringify(selected, null, 2)}\n`;
    await fs.writeFile(descriptorFile, selectedBytes);
    store.invalidate();
    const invalid = await readScope();
    assert.equal(invalid.status, "invalid");
    assert.ok(invalid.errors.some((error) => error.includes("application_consent_authority selection is retired")));
    assert.equal(invalid.application_consent_authority, undefined);
    const state = await readWorkspaceSetupState(options);
    assert.equal(state.status, "degraded");
    assert.ok(state.errors.some((error) => error.includes("application_consent_authority selection is retired")));
    assert.equal(await fs.readFile(descriptorFile, "utf8"), selectedBytes);
    const descriptorBytes = `${JSON.stringify(descriptor, null, 2)}\n`;
    await fs.writeFile(descriptorFile, descriptorBytes);
    const receiptFile = path.join(home, "client-state", "workspace", "receipts", `${request.operationId}.json`);
    const receipt = JSON.parse(await fs.readFile(receiptFile, "utf8"));
    delete receipt.publication_evidence;
    const transaction = path.join(home, "client-state", "workspace", "transactions", request.operationId);
    await fs.mkdir(path.join(transaction, "after"), { recursive: true });
    await fs.mkdir(path.join(transaction, "before"));
    const hash = (bytes) => crypto.createHash("sha256").update(bytes).digest("hex");
    const journalWrites = [];
    for (const write of receipt.writes) {
      const bytes = await fs.readFile(write.path);
      const selectedHome = write.path === descriptorFile;
      journalWrites.push({ destination: write.path, expected_before_digest: hash(bytes), before_digest: hash(bytes),
        after_digest: selectedHome ? hash(selectedBytes) : hash(bytes), after_file: selectedHome ? "after/home.bin" : "",
        before_file: selectedHome ? "before/home.bin" : "", tag: write.tag, publish_barrier: write.publish_barrier,
        state: selectedHome ? "staged" : "unchanged" });
    }
    await fs.writeFile(path.join(transaction, "after", "home.bin"), selectedBytes);
    await fs.writeFile(path.join(transaction, "before", "home.bin"), descriptorBytes);
    const journal = { schema: "octopus.workspace_setup_transaction", schema_version: 1, operation_id: request.operationId,
      request_digest: receipt.request_digest, phase: "staged", writes: journalWrites, receipt: { ...receipt,
        writes: journalWrites.map((write) => ({ path: write.destination, digest: write.after_digest, tag: write.tag, publish_barrier: write.publish_barrier })) } };
    const journalBytes = `${JSON.stringify(journal, null, 2)}\n`;
    await fs.writeFile(path.join(transaction, "journal.json"), journalBytes);
    await assert.rejects(recoverWorkspaceTransactions(options), /application_consent_authority selection is retired/);
    assert.equal(await fs.readFile(descriptorFile, "utf8"), descriptorBytes);
    assert.equal(await fs.readFile(path.join(transaction, "journal.json"), "utf8"), journalBytes);
    console.log("retired Home consent selection refuses setup, projection, and pending replay without conversion");
  } finally { store?.close(); await fs.rm(temporary, { recursive: true, force: true }); }
}

async function verifyFreshAgentRecipe(temporary) {
  temporary = await fs.realpath(temporary);
  const { planClientAgentTemplate, readClientAgentCatalog } = require("../electron/clientAgentCatalog.cjs");
  const sourceRoot = path.resolve(__dirname, "../../registry/interface/system_worker_computer_use");
  const sourceInterface = JSON.parse(await fs.readFile(path.join(sourceRoot, "interface.json")));
  const sourcePolicy = JSON.parse(await fs.readFile(path.join(sourceRoot, "policy.json")));
  for (const admitted of [true, false]) {
    const home = path.join(temporary, admitted ? "agent-recipe" : "agent-refused");
    const registry = path.join(home, "registry");
    await createFixture(home, registry);
    const templatePath = path.join(registry, "ee_template", "project", "ee_template.json");
    const template = JSON.parse(await fs.readFile(templatePath));
    template.allowed_interfaces = admitted ? [sourceInterface.id] : [];
    await fs.writeFile(templatePath, `${JSON.stringify(template, null, 2)}\n`);
    const request = setupRequest(admitted ? "recipe_admitted_001" : "recipe_refused_001");
    request.agentPresets = [{ packId: "fixture", expectedPackManifestDigest: "a".repeat(64), presetId: "computer-use", projectKeys: ["work"], providerInterfaceId: request.home.providerInterfaceId, providerModel: request.home.providerModel, reasoningEffort: "high" }];
    const options = {
      octopusHomeRoot: home, registryRoot: registry, request,
      planOperatorSurface: (input) => fixturePlan(home, registry, input),
      planAgentPreset: async ({ choice, projectEeId, operationId }) => ({
        ...planClientAgentTemplate({ operationId, projectEeId, sourceInterface, sourcePolicy, name: "Computer Use", icon: "goggles",
          sourceProfile: { ee_id: projectEeId, worker_choice_interface: sourceInterface.id, door_host_slots: {}, context_artifact_bindings: sourcePolicy.role_artifacts.map((id) => ({ id })), selected_worker_environment_kinds: ["desktop_session"] },
          settings: { schema_version: 1, worker_interface: sourceInterface.id, project_ee_id: projectEeId, provider_interface_id: choice.providerInterfaceId, reasoning_effort: choice.reasoningEffort, application_ids: [], worker_environment_kinds: ["desktop_session"] },
        }), verify: async () => {},
      }),
    };
    if (!admitted) {
      await assert.rejects(initializeWorkspace(options), /does not admit exact source worker/);
      assert.deepEqual(await fs.readdir(path.join(home, "state", "runs")), []);
      assert.equal((await readClientAgentCatalog(home)).exists, false);
      continue;
    }
    const result = await initializeWorkspace(options);
    const catalog = await readClientAgentCatalog(home);
    assert.equal(catalog.document.agents.length, 1);
    const agent = catalog.document.agents[0];
    const profile = JSON.parse(await fs.readFile(path.join(registry, "interface", result.homeInterfaceId, "octopus_os.json"))).task_authoring_profiles.find((row) => row.worker_choice_interface === agent.worker_interface);
    assert.deepEqual(profile.context_artifact_bindings.map((row) => row.id), sourcePolicy.role_artifacts);
    assert.deepEqual(JSON.parse(await fs.readFile(path.join(registry, "interface", agent.worker_interface, "policy.json"))), { ...sourcePolicy, id: agent.worker_interface });
    assert.deepEqual(JSON.parse(await fs.readFile(templatePath)).allowed_interfaces, [sourceInterface.id]);
    assert.equal(catalog.document.project_sources.find((row) => row.project_ee_id === agent.project_ee_id).source_worker_interface, request.task.workerInterfaceId);
    assert.equal((await fs.readdir(path.join(home, "state", "runs"))).length, 1);
  }
}

async function verifyExistingRootAttachment(temporary) {
  const fixture = path.join(temporary, "existing-root");
  await createFixture(fixture, path.join(fixture, "registry"));
  const octopusHomeRoot = await fs.realpath(fixture);
  const registryRoot = path.join(octopusHomeRoot, "registry");
  const rootPath = path.join(octopusHomeRoot, "state/ee_instances/ee_existence.json");
  const root = JSON.parse(await fs.readFile(rootPath, "utf8"));
  Object.assign(root, { goal: "Preserve the authored goal", config: { selected: ["keep"] }, policy_narrowing: { allowed_tools: ["file.read"] } });
  const before = `${JSON.stringify(root, null, 2)}\n`;
  await fs.writeFile(rootPath, before);
  const request = setupRequest("existing_root_12345678");
  request.root = { key: "root", existingEeId: root.ee_id, expectedEeDigest: crypto.createHash("sha256").update(before).digest("hex"), name: "Attached" };
  request.projects = [];
  request.home.workerInterfaceId = "system_worker_experiment";
  request.home.selectedWorkerEnvironmentKinds = ["git_worktree", "process_session"];
  const planOperatorSurface = async (input) => {
    assert.equal(await fs.readFile(path.join(input.eeInstancesRoot, "ee_existence.json"), "utf8"), before);
    assert.equal(input.request.eeId, root.ee_id);
    const selected = input.request.clientSurfaceRole === "home" ? request.home : request.task;
    assert.equal(input.request.workerInterfaceId, selected.workerInterfaceId);
    assert.deepEqual(input.request.selectedWorkerEnvironmentKinds, selected.selectedWorkerEnvironmentKinds);
    return fixturePlan(octopusHomeRoot, registryRoot, input);
  };
  const options = { octopusHomeRoot, registryRoot, request, planOperatorSurface };
  await assert.rejects(initializeWorkspace({ ...options, request: { ...request, root: { ...request.root, expectedEeDigest: "0".repeat(64) } } }), /changed before planning/);
  await assert.rejects(initializeWorkspace({ ...options, request: { ...request, root: { ...request.root, goal: "Rewrite" } } }), /unknown fields/);
  const foreign = JSON.stringify({ ...root, ee_id: "ee_foreign" });
  await fs.writeFile(rootPath, foreign);
  await assert.rejects(initializeWorkspace({ ...options, request: { ...request, root: { ...request.root, expectedEeDigest: crypto.createHash("sha256").update(foreign).digest("hex") } } }), /foreign identity/);
  await fs.writeFile(rootPath, before);
  await assert.rejects(initializeWorkspace({ ...options, planOperatorSurface: async (input) => {
    const plan = await planOperatorSurface(input);
    const write = plan.writes.find((row) => row.tag.startsWith("run:"));
    write.content = JSON.stringify({ ...JSON.parse(write.content), ee: "ee_foreign" });
    return plan;
  } }), /changed its run, EE/);
  await assert.rejects(initializeWorkspace({ ...options, planOperatorSurface: async (input) => {
    const plan = await planOperatorSurface(input);
    await fs.writeFile(rootPath, `${before}\n`);
    return plan;
  } }), /changed during initialization/);
  await fs.writeFile(rootPath, before);
  let drifted = false;
  await assert.rejects(initializeWorkspace({ ...options, onProgress: (event) => {
    if (event.phase === "applying" && !drifted) {
      require("node:fs").writeFileSync(rootPath, `${before}\n`);
      drifted = true;
    }
  } }), /changed during initialization/);
  assert.equal(drifted, true);
  assert.equal(await fs.readFile(rootPath, "utf8"), `${before}\n`);
  assert.deepEqual(await fs.readdir(path.join(octopusHomeRoot, "state/runs")), []);
  assert.equal((await readWorkspaceSetupState(options)).status, "not_configured");
  await fs.writeFile(rootPath, before);
  const outcome = await initializeWorkspace(options);
  assert.equal(outcome.rootEeId, root.ee_id);
  assert.deepEqual(await fs.readdir(path.dirname(rootPath)), ["ee_existence.json"]);
  assert.equal(await fs.readFile(rootPath, "utf8"), before);
  const state = await readWorkspaceSetupState(options);
  assert.equal(state.status, "configured");
  const artifactWrite = state.receipt.writes.find((write) => write.tag.startsWith("artifact:"));
  const artifactBytes = await fs.readFile(artifactWrite.path);
  assert.equal(crypto.createHash("sha256").update(artifactBytes).digest("hex"), artifactWrite.digest);
  assert.equal(JSON.parse(artifactBytes).text, "Exact fixture Home context");
  assert.equal(state.receipt.existing_root.digest, request.root.expectedEeDigest);
  assert.equal(state.receipt.writes.some((write) => write.path === rootPath), false);
  const runPath = path.join(octopusHomeRoot, "state/runs", `${outcome.homeRunId}.json`);
  const run = JSON.parse(await fs.readFile(runPath, "utf8"));
  run.status = "idle";
  run.checkpoints[0].epoch += 1;
  await fs.writeFile(runPath, JSON.stringify(run));
  assert.equal((await readWorkspaceSetupState(options)).status, "configured");
  assert.equal((await initializeWorkspace(options)).receiptDigest, outcome.receiptDigest);
  const receiptPath = path.join(octopusHomeRoot, "client-state/workspace/receipts", `${request.operationId}.json`);
  const receipt = state.receipt;
  receipt.existing_root.path = path.join(temporary, "foreign-home/state/ee_instances/ee_existence.json");
  await fs.writeFile(receiptPath, JSON.stringify(receipt));
  const refused = await readWorkspaceSetupState(options);
  assert.equal(refused.status, "degraded");
  assert.match(refused.errors.join("\n"), /another Home/);
}

async function verifyRootFirstSetup(temporary) {
  const { compileSetupPlan } = require("../cli/setup.cjs");
  const cliRecipe = setupRequest("cli_root_first_12345678");
  const recipeFor = (row) => ({ worker_interface_id: row.workerInterfaceId, provider_interface_id: row.providerInterfaceId,
    context_artifact_bindings: row.contextArtifactBindings, selected_worker_environment_kinds: row.selectedWorkerEnvironmentKinds });
  const cliPlan = compileSetupPlan({ id: "fixture", manifest_digest: "a".repeat(64), components: [],
    workspace_setup: { home: recipeFor(cliRecipe.home), task: recipeFor(cliRecipe.task), project_defaults: [] } },
  { operationId: cliRecipe.operationId, workspaceName: "Fresh", workspaceGoal: "Fresh", projects: [],
    model: "fixture-model", reasoningEffort: "high", recommended: [] });
  const cliHome = path.join(await fs.realpath(temporary), "cli-root-first"), cliRegistry = path.join(cliHome, "registry");
  await createFixture(cliHome, cliRegistry);
  const cliOptions = { octopusHomeRoot: cliHome, registryRoot: cliRegistry, request: cliPlan.workspace.request,
    planOperatorSurface: (input) => fixturePlan(cliHome, cliRegistry, input) };
  await initializeWorkspace(cliOptions);
  assert.equal((await readWorkspaceSetupState(cliOptions)).receipt.request_digest, cliPlan.workspace.request_digest);
  assert.equal((await initializeWorkspace(cliOptions)).changed, false);
  const filename = path.resolve(__dirname, "../src/onboarding/workspaceSetup.ts"), ts = require("typescript"), compiled = { exports: {} };
  const code = ts.transpileModule(await fs.readFile(filename, "utf8"), { fileName: filename,
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 } }).outputText;
  Function("module", "exports", "require", code)(compiled, compiled.exports, require("node:module").createRequire(filename));
  const { createWorkspaceSetupDraft, validateWorkspaceSetupDraft, compileWorkspaceInitializeRequest, workspaceInitializeRequestDigest } = compiled.exports;
  let sequence = 0;
  const draft = createWorkspaceSetupDraft("founder", () => `root_first_${++sequence}`);
  assert.equal(Object.hasOwn(draft, "providerModel"), false);
  assert.throws(() => validateWorkspaceSetupDraft({ ...draft, providerModel: 42 }), /Selected model must be text/);
  const missingGoal = { ...draft }; delete missingGoal.spaceGoal;
  assert.throws(() => validateWorkspaceSetupDraft(missingGoal), /missing fields: spaceGoal/);
  Object.assign(draft, { spaceName: "Chosen space", spaceGoal: "Chosen root goal", providerModel: "fixture-model", reasoningEffort: "high" });
  const recipe = setupRequest("recipe_root_12345678").home;
  const request = compileWorkspaceInitializeRequest(draft, { provider_interface_id: recipe.providerInterfaceId, worker_interface_id: recipe.workerInterfaceId,
    door_host_slots: { system_workspace_env: { workspace_root: "/", host_filesystem_root: "/" } },
    context_artifact_bindings: recipe.contextArtifactBindings, selected_worker_environment_kinds: recipe.selectedWorkerEnvironmentKinds, project_defaults: [],
    task: { worker_interface_id: setupRequest("recipe_root_12345678").task.workerInterfaceId,
      context_artifact_bindings: [], selected_worker_environment_kinds: ["git_worktree"] } });
  assert.deepEqual(request.root, { key: "root", templateName: "existence", name: draft.spaceName, goal: draft.spaceGoal });
  const home = path.join(await fs.realpath(temporary), "root-first"), registry = path.join(home, "registry");
  await createFixture(home, registry);
  await fs.rm(path.join(registry, "ee_template", "operator_space"), { recursive: true });
  const rootPath = path.join(home, "state", "ee_instances", "ee_existence.json"), templatePath = path.join(registry, "ee_template", "existence", "ee_template.json");
  const root = JSON.parse(await fs.readFile(rootPath));
  Object.assign(root, { config: { selected: ["preserved"] }, policy_narrowing: { allowed_worker_environment_kinds: ["git_worktree"] } });
  const before = `${JSON.stringify(root, null, 2)}\n`, template = await fs.readFile(templatePath);
  await fs.writeFile(rootPath, before);
  const options = { octopusHomeRoot: home, registryRoot: registry, request, planOperatorSurface: (input) => fixturePlan(home, registry, input) };
  await assert.rejects(initializeWorkspace({ ...options, request: { ...request, root: { ...request.root, templateName: "operator_space" } } }), /actual bootstrap-root/);
  await fs.writeFile(templatePath, JSON.stringify({ ...JSON.parse(template), bootstrap_root: false }));
  await assert.rejects(initializeWorkspace(options), /declared bootstrap-root/);
  await fs.writeFile(templatePath, JSON.stringify({ ...JSON.parse(template), allowed_child_ee_templates: [] }));
  await assert.rejects(initializeWorkspace(options), /does not allow template 'project'/);
  await fs.writeFile(templatePath, template);
  let drifted = false;
  await assert.rejects(initializeWorkspace({ ...options, planOperatorSurface: async (input) => {
    if (!drifted) { drifted = true; await fs.writeFile(rootPath, `${before}\n`); }
    return fixturePlan(home, registry, input);
  } }), /expected .*found/);
  assert.equal(await fs.readFile(rootPath, "utf8"), `${before}\n`);
  await fs.writeFile(rootPath, before);
  const open = fs.open;
  let failed = false;
  fs.open = async (file, flags, mode) => {
    if (!failed && flags === "wx" && file.endsWith(`${path.sep}interface.json`)) { failed = true; throw new Error("fixture publication write failed"); }
    return open(file, flags, mode);
  };
  try { await assert.rejects(initializeWorkspace(options), /fixture publication write failed/); }
  finally { fs.open = open; }
  assert.equal(failed, true);
  assert.equal(await fs.readFile(rootPath, "utf8"), before);
  assert.deepEqual(await fs.readdir(path.join(home, "state", "runs")), []);
  assert.deepEqual(await fs.readdir(path.dirname(rootPath)), ["ee_existence.json"]);
  const outcome = await initializeWorkspace(options), state = await readWorkspaceSetupState(options);
  assert.equal(outcome.rootEeId, root.ee_id);
  assert.deepEqual(JSON.parse(await fs.readFile(rootPath)), { ...root, name: draft.spaceName, goal: draft.spaceGoal });
  assert.equal(state.receipt.request_digest, await workspaceInitializeRequestDigest(request));
  assert.equal(state.status, "configured");
  assert.equal((await fs.readdir(path.dirname(rootPath))).length, request.projects.length + 1);
  const projectIds = new Map(outcome.projects.map((project) => [project.key, project.eeId]));
  for (const project of request.projects) {
    const record = JSON.parse(await fs.readFile(path.join(path.dirname(rootPath), `${projectIds.get(project.key)}.json`)));
    assert.equal(record.name, project.name);
    assert.equal(record.parent_ee_id, project.parentKey === "root" ? root.ee_id : projectIds.get(project.parentKey));
    assert.equal(record.goal, project.goal);
  }
  const sidecar = JSON.parse(await fs.readFile(path.join(registry, "interface", outcome.homeInterfaceId, "octopus_os.json")));
  assert.deepEqual(sidecar.authoring_profile.door_host_slots, request.home.doorHostSlots);
  assert.ok(sidecar.task_authoring_profiles.every((profile) => Object.keys(profile.door_host_slots).length === 0));
  assert.equal(sidecar.presentation.name, draft.spaceName);
  assert.deepEqual(sidecar.presentation.projects, request.projects.map((project) => ({ ee_id: projectIds.get(project.key), name: project.name, ...project.presentation })));
  assert.equal((await initializeWorkspace(options)).changed, false);
  await fs.writeFile(rootPath, JSON.stringify({ ...root, parent_ee_id: "foreign_root" }));
  assert.equal((await readWorkspaceSetupState(options)).status, "degraded");
}

async function verifyMaintenanceSetup(temporary) {
  const fixture = path.join(temporary, "maintenance-setup");
  await createFixture(fixture, path.join(fixture, "registry"));
  const home = await fs.realpath(fixture), registry = path.join(home, "registry"), runsRoot = path.join(home, "state", "runs");
  const owner = path.join(registry, "interface", "world_operator_chat");
  await fs.mkdir(owner, { recursive: true });
  await fs.copyFile(path.resolve(__dirname, "../../registry/interface/world_operator_chat/maintenance-client.cjs"), path.join(owner, "maintenance-client.cjs"));
  const { maintenanceFixture } = require("../../registry/interface/world_operator_chat/test_maintenance_client.cjs");
  const request = setupRequest("maintenance_setup_12345678"); request.projects = [];
  let invalid = "";
  const options = { octopusHomeRoot: home, registryRoot: registry, request, planOperatorSurface: async (input) => {
    const plan = await fixturePlan(home, registry, input);
    if (input.request.clientSurfaceRole !== "home") return plan;
    const source = plan.writes.find((write) => write.tag === `run:${plan.runId}`);
    const native = maintenanceFixture(registry, runsRoot, {
      run_id: plan.runId, chat_interface_id: plan.chatInterfaceId, model_interface_id: plan.modelInterfaceId, worker_interface_id: plan.workerInterfaceId,
      run_record_content: source.content, interface_writes: plan.writes.filter((write) => write !== source).map((write) => ({
        path: write.filePath, content: write.content, tag: write.tag, expected_before_hash: write.expectedBeforeHash,
      })),
    });
    plan.requiredInterfaceIds = [...new Set([...plan.requiredInterfaceIds, ...native.interface_writes.filter((write) => path.basename(write.path) === "interface.json").map((write) => JSON.parse(write.content).id)])].sort();
    plan.requiredToolIds = [...new Set([...plan.requiredToolIds, ...native.interface_writes.filter((write) => path.basename(write.path) === "tool.json").map((write) => JSON.parse(write.content).id)])].sort();
    plan.writes = [...native.interface_writes, ...native.run_writes, { path: source.filePath, content: native.run_record_content, tag: source.tag, expected_before_hash: null }]
      .map((write) => ({ filePath: write.path, content: write.content, tag: write.tag, expectedBeforeHash: write.expected_before_hash }));
    if (invalid === "missing") plan.writes = plan.writes.filter((write) => write.tag !== "run:projection_run");
    if (invalid === "ceiling" || invalid === "source-ceiling") {
      const policy = plan.writes.find((write) => write.tag === `policy:${invalid === "ceiling" ? "observer_worker" : plan.workerInterfaceId}`);
      const value = JSON.parse(policy.content);
      if (invalid === "ceiling") value.ceiling.tools = [];
      else value.ceiling.interfaces = value.ceiling.interfaces.filter((id) => id !== "terminal_relay");
      policy.content = JSON.stringify(value);
    }
    if (invalid === "bytes") plan.writes.find((write) => write.tag === "run:observer_run").content += "\n";
    if (invalid === "extra") plan.writes.push({ filePath: path.join(runsRoot, "undeclared.json"), tag: "run:undeclared", expectedBeforeHash: null,
      content: JSON.stringify({ ...JSON.parse(source.content), id: "undeclared", checkpoints: [], active_checkpoints: [] }) });
    return plan;
  } };
  for (const [mode, reason] of [["missing", /declared initial bytes/], ["ceiling", /private tool inventory differs from companion ceilings/],
    ["source-ceiling", /source terminal relay is outside the source worker ceiling/], ["bytes", /declared initial bytes/], ["extra", /undeclared Run/]]) {
    invalid = mode; await assert.rejects(initializeWorkspace(options), reason);
    assert.deepEqual(await fs.readdir(runsRoot), []);
  }
  invalid = "";
  const result = await initializeWorkspace(options);
  assert.equal(result.changed, true);
  assert.equal((await fs.readdir(runsRoot)).length, 5);
  const record = JSON.parse(await fs.readFile(path.join(runsRoot, `${result.homeRunId}.json`), "utf8"));
  const sourcePolicy = JSON.parse(await fs.readFile(path.join(registry, "interface", record.worker, "policy.json")));
  assert.equal(sourcePolicy.ceiling.tools.includes("file.write.publication"), false);
  assert.equal(sourcePolicy.ceiling.interfaces.includes("observer_provider"), false);
  assert.equal(JSON.parse(await fs.readFile(path.join(runsRoot, "observer_run.json"))).worker, "observer_worker");
  assert.equal(JSON.parse(await fs.readFile(path.join(runsRoot, "projection_run.json"))).worker, "projection_worker");
  console.log("maintenance setup: exact companions and writers, independent ceilings, partial/foreign/changed plan refusal passed");
}

async function fixturePlan(octopusHomeRoot, registryRoot, { eeInstancesRoot, request }) {
  const rootRecord = JSON.parse(await fs.readFile(path.join(eeInstancesRoot, `${request.eeId}.json`), "utf8"));
  const clientSurfaceRole = request.clientSurfaceRole ?? "home";
  if (clientSurfaceRole === "home") {
    assert.equal(rootRecord.parent_ee_id, null, "Home must live directly in the root Existence EE; select its actual root EE");
    const template = JSON.parse(await fs.readFile(path.join(registryRoot, "ee_template", rootRecord.template_name, "ee_template.json")));
    assert.equal(template.bootstrap_root, true, "Home requires the declared bootstrap-root EE");
  } else if (rootRecord.parent_ee_id !== null) {
    const parent = JSON.parse(await fs.readFile(path.join(eeInstancesRoot, `${rootRecord.parent_ee_id}.json`)));
    const template = JSON.parse(await fs.readFile(path.join(registryRoot, "ee_template", parent.template_name, "ee_template.json")));
    assert.ok(template.allowed_child_ee_templates.includes(rootRecord.template_name));
  }
  const runId = `run_operator_chat_${request.chatId}`;
  const chatInterfaceId = `world_operator_chat_${request.chatId}`;
  const modelInterfaceId = `${request.providerInterfaceId}_${request.chatId}`;
  const workerChoiceInterfaceId = request.workerInterfaceId;
  const workerInterfaceId = `${workerChoiceInterfaceId}_${request.chatId}`;
  const toolId = `file.write__authoring_${request.chatId}`;
  const privateId = crypto.createHash("sha256").update(request.chatId).digest("hex").slice(0, 24);
  const intakeId = `system_service_input_${privateId}`;
  const writerId = `system_input_writer_${privateId}`;
  const inputToolId = `file.write.input_${privateId}`;
  const requiredInterfaceIds = [chatInterfaceId, modelInterfaceId, workerInterfaceId, intakeId, writerId].sort();
  const requiredToolIds = [toolId, inputToolId].sort();
  const authoringProfile = {
    ee_id: request.eeId,
    worker_choice_interface: workerChoiceInterfaceId,
    door_host_slots: request.doorHostSlots ?? {},
    ...(request.contextArtifactBindings?.length ? { context_artifact_bindings: request.contextArtifactBindings } : {}),
    ...(request.payloadArtifactBindings?.length ? { payload_artifact_bindings: request.payloadArtifactBindings } : {}),
    ...(request.externalRelays?.length ? { external_relays: request.externalRelays.map((route) => ({
      checkpoint_id: route.checkpointId, interface_id: route.interfaceId, outbound_interface_ids: route.outboundInterfaceIds,
    })) } : {}),
    ...(request.selectedWorkerEnvironmentKinds?.length
      ? { selected_worker_environment_kinds: request.selectedWorkerEnvironmentKinds }
      : {}),
  };
  const interfaceRoot = path.join(registryRoot, "interface", chatInterfaceId);
  const sidecar = {
    schema_version: 1,
    workspace: "octopus_os",
    interface: chatInterfaceId,
    owner_interface: chatInterfaceId,
    component: "client",
    surface_role: clientSurfaceRole,
    provider_choice_interface: request.providerInterfaceId,
    worker_choice_interface: workerChoiceInterfaceId,
    authoring_profile: authoringProfile,
  };
  const interfaceBinding = (interfaceId) => ({
    interface: interfaceId,
    config: { artifact_policy: { mode: "all" } },
  });
  const checkpoints = [
    checkpoint("cp_home_input", chatInterfaceId, [modelInterfaceId]),
    checkpoint("cp_worker_input", workerInterfaceId, [modelInterfaceId]),
    checkpoint("cp_home_output", modelInterfaceId, [chatInterfaceId]),
    checkpoint("cp_intake_input", intakeId, [modelInterfaceId]),
    checkpoint("cp_intake_call", intakeId, [writerId]),
    checkpoint("cp_intake_result", writerId, [intakeId]),
  ];
  const artifactId = `artifact.fixture.home.${request.chatId}`;
  const artifactWrites = clientSurfaceRole === "home" ? [write(path.join(registryRoot, "artifact", artifactId, "artifact.json"),
    `${JSON.stringify({ id: artifactId, label: "context.reference", text: "Exact fixture Home context" })}\n`, `artifact:${artifactId}`)] : [];
  if (artifactWrites.length) checkpoints[0].artifact_bindings = [{ id: artifactId }];
  checkpoints[4].required_verified_artifacts = [{ artifact_label: "tool.call", mark: "tool.call.allowed", verifier: "tool_call_allowed", params: { tools: [inputToolId], metadata_matches: [{ payload: "/arguments/expected_before_hash", metadata: "/runtime/run_record_hash" }] } }];
  const runRecord = {
    id: runId,
    ee: request.eeId,
    worker: workerInterfaceId,
    trace: `trace:${runId}`,
    status: "running",
    interface_state: {
      [chatInterfaceId]: {
        client_surface_role: clientSurfaceRole,
        provider_choice_interface: request.providerInterfaceId,
        provider_runtime_interface: modelInterfaceId,
        worker_choice_interface: workerChoiceInterfaceId,
      },
    },
    checkpoints,
    active_checkpoints: checkpoints.map((entry) => entry.id),
  };
  function checkpoint(id, inbound, outbound) {
    return {
      id,
      run: runId,
      trace: `trace:${runId}:${id}`,
      epoch: 0,
      interface_binding: interfaceBinding(inbound),
      outbound_interface_bindings: outbound.map(interfaceBinding),
      artifact_bindings: [],
      required_verified_artifacts: [],
    };
  }
  return {
    runId,
    chatInterfaceId,
    modelInterfaceId,
    providerModel: request.providerModel,
    workerInterfaceId,
    clientSurfaceRole,
    reasoningEffort: request.reasoningEffort ?? "high",
    serviceTier: request.serviceTier ?? "",
    authoringProfile,
    requiredInterfaceIds,
    requiredToolIds,
    writes: [
      ...artifactWrites,
      write(path.join(interfaceRoot, "interface.json"), `${JSON.stringify({ id: chatInterfaceId })}\n`, `interface:${chatInterfaceId}`),
      write(path.join(interfaceRoot, "octopus_os.json"), `${JSON.stringify(sidecar, null, 2)}\n`, `octopus-os:${chatInterfaceId}`),
      write(path.join(registryRoot, "interface", modelInterfaceId, "interface.json"), `${JSON.stringify({ id: modelInterfaceId, interface_driver_config: { private_config: { model: request.providerModel } } })}\n`, `interface:${modelInterfaceId}`),
      write(path.join(registryRoot, "interface", workerInterfaceId, "interface.json"), `${JSON.stringify({ id: workerInterfaceId })}\n`, `interface:${workerInterfaceId}`),
      write(path.join(registryRoot, "interface", workerInterfaceId, "policy.json"), `${JSON.stringify({ id: workerInterfaceId, ceiling: { interfaces: requiredInterfaceIds, tools: requiredToolIds } })}\n`, `policy:${workerInterfaceId}`),
      write(path.join(registryRoot, "interface", intakeId, "interface.json"), JSON.stringify({ id: intakeId, role: "service" }), `interface:${intakeId}`),
      write(path.join(registryRoot, "interface", intakeId, "life.json"), JSON.stringify({}), `life:${intakeId}`),
      write(path.join(registryRoot, "interface", writerId, "interface.json"), JSON.stringify({ id: writerId, role: "system" }), `interface:${writerId}`),
      write(path.join(registryRoot, "tool", toolId, "tool.json"), `${JSON.stringify({ id: toolId, interface: writerId })}\n`, `tool:${toolId}`),
      write(path.join(registryRoot, "tool", inputToolId, "tool.json"), JSON.stringify({ id: inputToolId, interface: writerId }), `tool:${inputToolId}`),
      write(path.join(octopusHomeRoot, "state", "runs", `${runId}.json`), `${JSON.stringify(runRecord, null, 2)}\n`, `run:${runId}`),
    ],
    ledgerOffset: 0,
  };
}

async function fixtureProjectScope(octopusHomeRoot, registryRoot, workspace) {
  const environments = [];
  const eeRoot = path.join(octopusHomeRoot, "state", "ee_instances");
  for (const entry of await fs.readdir(eeRoot, { withFileTypes: true })) {
    if (!entry.isFile() || !entry.name.endsWith(".json")) continue;
    const bytes = await fs.readFile(path.join(eeRoot, entry.name));
    const record = JSON.parse(bytes.toString("utf8"));
    const template = JSON.parse(await fs.readFile(path.join(registryRoot, "ee_template", record.template_name, "ee_template.json"), "utf8"));
    environments.push({
      eeId: record.ee_id,
      parentEeId: record.parent_ee_id,
      templateName: record.template_name,
      recordDigest: crypto.createHash("sha256").update(bytes).digest("hex"),
      allowedChildTemplateIds: [...template.allowed_child_ee_templates].sort(),
    });
  }
  const sidecar = await fs.readFile(path.join(registryRoot, "interface", workspace.homeInterfaceId, "octopus_os.json"));
  return {
    status: "ready",
    rootEeId: workspace.rootEeId,
    homeRunId: workspace.homeRunId,
    homeInterfaceId: workspace.homeInterfaceId,
    homeSidecarDigest: crypto.createHash("sha256").update(sidecar).digest("hex"),
    environments,
    clientRuns: [{ runId: workspace.homeRunId, eeId: workspace.rootEeId }],
    readOnlyRuns: [],
  };
}

function fixtureWorkspaceIdentity(scope) {
  return {
    rootEeId: scope.rootEeId,
    homeRunId: scope.homeRunId,
    homeInterfaceId: scope.homeInterfaceId,
    expectedHomeSidecarDigest: scope.homeSidecarDigest,
  };
}

async function createFixture(octopusHomeRoot, registryRoot) {
  for (const directory of [
    path.join(octopusHomeRoot, "state", "ee_instances"),
    path.join(registryRoot, "interface"),
    path.join(registryRoot, "tool"),
    path.join(registryRoot, "artifact"),
  ]) await fs.mkdir(directory, { recursive: true });
  await fs.writeFile(path.join(octopusHomeRoot, "state", "ee_instances", "ee_existence.json"), `${JSON.stringify({
    ee_id: "ee_existence",
    template_name: "existence",
    parent_ee_id: null,
    goal: "existence",
    config: {},
  }, null, 2)}\n`);
  await writeTemplate(registryRoot, "existence", ["project"]);
  await writeTemplate(registryRoot, "operator_space", ["project"]);
  await writeTemplate(registryRoot, "project", ["project"]);
}

async function writeTemplate(registryRoot, id, children) {
  const directory = path.join(registryRoot, "ee_template", id);
  await fs.mkdir(directory, { recursive: true });
  await fs.writeFile(path.join(directory, "ee_template.json"), `${JSON.stringify({
    id,
    goal_class: id,
    allowed_interfaces: null,
    allowed_worker_environment_kinds: null,
    allowed_child_ee_templates: children,
    bootstrap_root: id === "existence",
  }, null, 2)}\n`);
}

function setupRequest(operationId) {
  return {
    action: "initialize_workspace",
    operationId,
    root: { key: "root", templateName: "existence", name: "Founder", goal: "Operate life and work" },
    home: {
      name: "Home",
      providerInterfaceId: "world_provider_codex",
      providerModel: "fixture-model",
      workerInterfaceId: "system_worker_coder_max",
      reasoningEffort: "high",
      doorHostSlots: {},
      contextArtifactBindings: [{ id: "artifact-grounding-system-architecture" }],
      selectedWorkerEnvironmentKinds: ["git_worktree"],
    },
    task: {
      workerInterfaceId: "system_worker_fixture_task",
      contextArtifactBindings: [{ id: "artifact-worker-role-operator-task" }],
      selectedWorkerEnvironmentKinds: ["git_worktree"],
    },
    projects: [
      project("life", "root", "Life", "globe", "teal"),
      project("work", "root", "Work", "briefcase", "indigo"),
      project("product", "work", "Product", "rocket", "violet"),
    ],
  };
}

function project(key, parentKey, name, icon, color) {
  return { key, parentKey, templateName: "project", name, goal: name, presentation: { icon, color } };
}

function write(filePath, content, tag) {
  return { filePath, content, tag, expectedBeforeHash: null };
}

function deterministicId(prefix, operationId, key) {
  return `${prefix}_${crypto.createHash("sha256").update(`${operationId}\u0000${key}`).digest("hex").slice(0, 24)}`;
}

async function verifyProjectPolicies() {
  const home = await fs.realpath(await fs.mkdtemp(path.join(os.tmpdir(), "octopus-project-policy-test-"))), registry = path.join(home, "registry");
  try {
    await createFixture(home, registry);
    const planOperatorSurface = (input) => fixturePlan(home, registry, input);
    const workspace = await initializeWorkspace({ octopusHomeRoot: home, registryRoot: registry, request: setupRequest("policy_setup_12345678"), planOperatorSurface });
    await fs.mkdir(path.join(home, "state/ee_instances/.octopus-publication"), { mode: 0o700 });
    await writeTemplate(registry, "existence", ["project", "operator_space", "existence"]);
    const scope = await fixtureProjectScope(home, registry, workspace);
    const request = { action: "create_project", operationId: "policy_create_12345678", workspace: fixtureWorkspaceIdentity(scope), parentEeId: workspace.rootEeId,
      expectedParentDigest: scope.environments.find((row) => row.eeId === workspace.rootEeId).recordDigest,
      sourceFolders: [home, registry], templateName: "operator_space", name: "General work", goal: "A concrete goal", presentation: { icon: "folder", color: "blue" } };
    const options = { octopusHomeRoot: home, registryRoot: registry, scope, request, planOperatorSurface };
    const before = await fs.readdir(path.join(home, "state/ee_instances"));
    await assert.rejects(mutateWorkspaceProject({ ...options, request: { ...request, templateName: "missing" } }), /does not allow/);
    await assert.rejects(mutateWorkspaceProject({ ...options, request: { ...request, templateName: "existence" } }), /bootstrap-root/);
    await writeTemplate(registry, "existence", ["project"]);
    await assert.rejects(mutateWorkspaceProject(options), /no longer allows/);
    await writeTemplate(registry, "existence", ["project", "operator_space", "existence"]);
    await assert.rejects(mutateWorkspaceProject({ ...options, planOperatorSurface: async () => { throw new Error("formed child policy refusal"); } }), /formed child policy refusal/);
    assert.deepEqual(await fs.readdir(path.join(home, "state/ee_instances")), before);
    await assert.rejects(mutateWorkspaceProject({ ...options, planOperatorSurface: async (input) => {
      const plan = await planOperatorSurface(input); await writeTemplate(registry, "operator_space", []); return plan;
    } }), /policy changed during creation/);
    await writeTemplate(registry, "operator_space", ["project"]);
    const created = await mutateWorkspaceProject(options);
    const child = JSON.parse(await fs.readFile(path.join(home, "state/ee_instances", `${created.eeId}.json`)));
    assert.equal(child.name, "General work");
    assert.equal(child.goal, "A concrete goal");
    assert.deepEqual(child.config.octopus_os.source_folders, [home, registry]);
    assert.equal(child.template_name, "operator_space"); assert.equal(child.parent_ee_id, workspace.rootEeId);
    assert.equal((await mutateWorkspaceProject(options)).changed, false);
    const editScope = await fixtureProjectScope(home, registry, workspace);
    const homeSidecar = JSON.parse(await fs.readFile(path.join(registry, "interface", workspace.homeInterfaceId, "octopus_os.json")));
    const edit = { action: "update_project_presentation", operationId: "folder_edit_12345678",
      workspace: fixtureWorkspaceIdentity(editScope),
      projects: homeSidecar.presentation.projects.map(({ ee_id, icon, color }) => ({ eeId: ee_id, icon, color,
        ...(ee_id === created.eeId ? { name: "Renamed environment", sourceFolders: [registry, home], expectedProjectDigest: editScope.environments.find((row) => row.eeId === ee_id).recordDigest } : {}) })) };
    await assert.rejects(mutateWorkspaceProject({ ...options, scope: editScope, request: { ...edit,
      projects: edit.projects.map((project) => project.eeId === created.eeId ? { ...project, expectedProjectDigest: undefined } : project) } }), /Project digest/);
    await assert.rejects(mutateWorkspaceProject({ ...options, scope: editScope, request: { ...edit,
      projects: edit.projects.map((project) => project.eeId === created.eeId ? { ...project, expectedProjectDigest: "0".repeat(64) } : project) } }), /Project changed/);
    await mutateWorkspaceProject({ ...options, scope: editScope, request: edit });
    const edited = JSON.parse(await fs.readFile(path.join(home, "state/ee_instances", `${created.eeId}.json`)));
    assert.deepEqual(edited.config.octopus_os.source_folders, [registry, home]);
    assert.equal(edited.name, "Renamed environment");
    assert.equal(edited.ee_id, child.ee_id);
    assert.equal(edited.template_name, child.template_name);
    assert.deepEqual(edited.policy_narrowing, child.policy_narrowing);
    assert.equal(edited.goal, child.goal);
    assert.equal((await mutateWorkspaceProject({ ...options, scope: editScope, request: edit })).changed, false);
    const sidecarPath = path.join(registry, "interface", workspace.homeInterfaceId, "octopus_os.json");
    const sidecarBytes = await fs.readFile(sidecarPath);
    const aliased = JSON.parse(sidecarBytes);
    aliased.presentation.projects.find((project) => project.ee_id === created.eeId).name = "Client-only alias";
    await fs.writeFile(sidecarPath, JSON.stringify(aliased));
    await fs.mkdir(path.join(home, "ledger"), { recursive: true });
    await fs.writeFile(path.join(home, "ledger/movements.jsonl"), "");
    const store = createProjectionStore({ resolveRepoRoot: () => path.resolve(__dirname, "../.."), sourceRegistry: () => registry,
      octopusHome: () => home, projectEePolicies: () => fixturePolicyProjection(home) });
    try {
      const topology = await store.read({ contract_version: 1, method: "projection.runtime.topology", params: { include_ledger: false } });
      const environment = topology.eeRaw.ee_instances.find((row) => row.ee_id === created.eeId);
      assert.equal(environment.name, edited.name);
      assert.equal(environment.goal, child.goal);
      assert.equal(environment.display_name, undefined);
      const scope = await store.read({ contract_version: 1, method: "projection.operator.os.scope" });
      assert.equal(scope.projects.find((row) => row.ee_id === created.eeId).name, edited.name);
    } finally { store.close(); await fs.writeFile(sidecarPath, sidecarBytes); }
    const health = await readWorkspaceSetupState({ octopusHomeRoot: home, registryRoot: registry });
    assert.equal(health.status, "configured", health.errors.join("; "));
    await fs.writeFile(path.join(home, "state/ee_instances/ee_unreceipted.json"), JSON.stringify({ ...child, ee_id: "ee_unreceipted" }));
    const orphaned = await readWorkspaceSetupState({ octopusHomeRoot: home, registryRoot: registry });
    assert.equal(orphaned.status, "degraded"); assert.match(orphaned.errors.join("; "), /operator spaces without receipts: ee_unreceipted/);
    console.log("project policy authoring: admitted preset, refusal, drift and replay passed");
  } finally { await fs.rm(home, { recursive: true, force: true }); }
}

async function verifySetupFile() {
  const ts = require("typescript"), moduleApi = require("node:module");
  const compile = async (relative, mocks = {}) => {
    const filename = path.resolve(__dirname, relative), compiled = { exports: {} };
    const code = ts.transpileModule(await fs.readFile(filename, "utf8"), { fileName: filename,
      compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 } }).outputText;
    const local = moduleApi.createRequire(filename);
    Function("module", "exports", "require", code)(compiled, compiled.exports, (id) => id in mocks ? mocks[id] : local(id));
    return compiled.exports;
  };
  const setup = await compile("../src/onboarding/workspaceSetup.ts");
  const colors = await compile("../src/design-system/projectColors.ts");
  const files = await compile("../src/onboarding/setupFile.ts", { "./workspaceSetup": setup, "../design-system/projectColors": colors });
  let sequence = 0;
  const ids = () => `setup_file_${++sequence}`;
  const source = setup.createWorkspaceSetupDraft("founder", ids);
  const [life, work, nested] = source.projects;
  const catalog = [
    { id: "codex", kind: "pack", name: "Codex Pack", included: true, projectSelectable: false },
    { id: "world_email", kind: "application", name: "Email", included: false, projectSelectable: true },
    { id: "driver.browser", kind: "driver", name: "Browser", included: false, projectSelectable: false },
  ];
  const provider = { models: [{ model: "fixture-model", displayName: "Fixture", description: "", defaultReasoningEffort: "medium",
    supportedReasoningEfforts: ["low", "medium", "high"], showInPicker: true, isDefault: true }] };
  Object.assign(source, { providerModel: "fixture-model", reasoningEffort: "high", capabilities: [
    { id: "codex", kind: "pack", selected: true, included: true, projectKeys: [] },
    { id: "world_email", kind: "application", selected: true, included: false, projectKeys: [work.key, nested.key] },
    { id: "driver.browser", kind: "driver", selected: false, included: false, projectKeys: [] },
  ] });
  const file = files.parseSetupFile(files.serializeSetupFile(files.setupFileFromDraft(source)));
  assert.deepEqual(file.projects.map((project) => [project.id, project.parent]), [["life", null], ["work", null], ["project-1", "work"], ["project-2", "work"], ["project-3", "work"]]);
  assert.deepEqual(file.capabilities, [{ kind: "application", id: "world_email", projects: ["project-1", "work"] }, { kind: "pack", id: "codex", projects: [] }]);
  assert.deepEqual(file.model, { id: "fixture-model", reasoning_effort: "high" });
  const base = setup.createWorkspaceSetupDraft("life", ids);
  const imported = files.draftFromSetupFile(file, base, catalog, provider, ids);
  assert.equal(imported.operationId, base.operationId);
  assert.equal(imported.preset, "custom");
  assert.deepEqual(imported.projects.map((project) => project.name), source.projects.map((project) => project.name));
  const keyOf = (name) => imported.projects.find((project) => project.name === name).key;
  const email = imported.capabilities.find((capability) => capability.id === "world_email");
  assert.deepEqual(new Set(email.projectKeys), new Set([keyOf(work.name), keyOf(nested.name)]));
  assert.equal(imported.capabilities.find((capability) => capability.id === "driver.browser").selected, false);
  assert.equal(imported.providerModel, "fixture-model");
  assert.equal(life.parentKey, null);
  assert.throws(() => files.draftFromSetupFile({ ...file, capabilities: [...file.capabilities, { kind: "application", id: "world_telegram", projects: [] }] }, base, catalog, provider, ids),
    /needs 'world_telegram', which this Octopus does not offer/);
  assert.throws(() => files.draftFromSetupFile({ ...file, model: { id: "missing-model" } }, base, catalog, provider, ids), /model 'missing-model', which is not available here/);
  assert.throws(() => files.parseSetupFile(JSON.stringify({ ...file, schema: "other" })), /not an Octopus setup file/);
  assert.throws(() => files.parseSetupFile(JSON.stringify({ ...file, extra: true })), /fields Octopus does not read: extra/);
  const home = files.setupFileFromHome({
    scope: { status: "ready", root_ee_id: "ee_root", root_environment: { name: "Lab", goal: "Run experiments" }, projects: [
      { ee_id: "ee_root", parent_ee_id: "", name: "Lab", goal: "Run experiments" },
      { ee_id: "ee_b", parent_ee_id: "ee_a", name: "Beta", goal: "" },
      { ee_id: "ee_a", parent_ee_id: "ee_root", name: "Alpha", goal: "Alpha goal" },
    ], project_presentations: [{ ee_id: "ee_a", name: "Alpha", icon: "rocket", color: "purple" }] },
    localPackages: { packs: [{ id: "codex", workspace_setup: { project_defaults: [{ package_id: "world_provider_codex" }], agent_presets: [] },
      components: [
        { package_id: "world_cron", selection: "recommended", product_kind: "application", installed: true, receipt_state: "valid" },
        { package_id: "world_system_review", selection: "recommended", product_kind: "application", installed: false, receipt_state: "missing" },
        { package_id: "world_provider_codex", selection: "included", product_kind: "application", installed: true, receipt_state: "valid" },
      ], extensions: [{ extension_id: "builtin.repair" }] }] },
    applications: { applications: [{ package_id: "world_provider_codex", project_states: [
      { ee_id: "ee_root", is_root: true, policy_allowed: true }, { ee_id: "ee_a", policy_allowed: true }, { ee_id: "ee_b", policy_allowed: false },
    ] }] },
    drivers: { drivers: [] },
    installedExtensionIds: new Set(["builtin.repair"]),
  });
  assert.deepEqual(home.space, { name: "Lab", goal: "Run experiments" });
  assert.deepEqual(home.projects, [
    { id: "alpha", parent: null, name: "Alpha", goal: "Alpha goal", icon: "rocket", color: "violet" },
    { id: "beta", parent: "alpha", name: "Beta", goal: "Beta", icon: "briefcase", color: "black" },
  ]);
  assert.deepEqual(home.capabilities, [
    { kind: "application", id: "world_cron", projects: [] },
    { kind: "application", id: "world_provider_codex", projects: ["alpha"] },
    { kind: "extension", id: "builtin.repair", projects: [] },
    { kind: "pack", id: "codex", projects: [] },
  ]);
}

const execution = process.argv[2] === "conditioned-recover"
  ? recoverWorkspaceTransactions({ octopusHomeRoot: process.argv[3], registryRoot: path.join(process.argv[3], "registry") })
  : process.argv[2] === "publication-recovery"
  ? fs.mkdtemp(path.join(os.tmpdir(), "octopus-publication-test-")).then(async temporary => {
    try { await verifyPublicationRecovery(await fs.realpath(temporary)); }
    finally { await fs.rm(temporary, { recursive: true, force: true }); }
  })
  : process.argv[2] === "home-recipe"
  ? fs.mkdtemp(path.join(os.tmpdir(), "octopus-home-recipe-test-")).then(async temporary => {
    try { await verifyRootFirstSetup(await fs.realpath(temporary)); } finally { await fs.rm(temporary, { recursive: true, force: true }); }
  })
  : process.argv[2] === "conditioned-publication"
  ? fs.mkdtemp(path.join(os.tmpdir(), "octopus-conditioned-test-")).then(async temporary => {
    try { await verifyConditionedPublicationRecovery(await fs.realpath(temporary)); }
    finally { await fs.rm(temporary, { recursive: true, force: true }); }
  })
  : process.argv[2] === "directory-recover"
  ? recoverWorkspaceTransactions(retirementFixtureOptions(process.argv[3]))
  : process.argv[2] === "task-removal"
  ? fs.mkdtemp(path.join(os.tmpdir(), "octopus-task-removal-test-")).then(async (temporary) => {
    try { await verifyLinkedTaskRemoval(await fs.realpath(temporary)); }
    finally { await fs.rm(temporary, { recursive: true, force: true }); }
  })
  : process.argv[2] === "setup-file"
  ? verifySetupFile()
  : process.argv[2] === "project-policy"
  ? verifyProjectPolicies()
  : process.argv[2] === "task-archive"
  ? fs.mkdtemp(path.join(os.tmpdir(), "octopus-task-archive-test-")).then(async (temporary) => {
      try { await verifyTaskArchive(temporary); } finally { await fs.rm(temporary, { recursive: true, force: true }); }
    })
  : process.argv[2] === "capability-preferences"
  ? verifyCapabilityPreferenceSetup()
  : process.argv[2] === "consent-setup"
  ? verifyRetiredConsentSetup()
  : process.argv[2] === "maintenance-setup"
  ? fs.mkdtemp(path.join(os.tmpdir(), "octopus-maintenance-setup-test-")).then(async (temporary) => {
    try { await verifyMaintenanceSetup(await fs.realpath(temporary)); } finally { await fs.rm(temporary, { recursive: true, force: true }); }
  })
  : process.argv[2] === "crash-child"
  ? crashChild(process.argv[3], process.argv[4])
  : process.argv[2] === "hold-workspace-child"
  ? crashChild(process.argv[3], process.argv[4], true)
  : process.argv[2] === "workspace-lock"
  ? fs.mkdtemp(path.join(os.tmpdir(), "octopus-workspace-lock-test-")).then(async temporary => {
    try { await verifyLiveWorkspaceLock(await fs.realpath(temporary)); } finally { await fs.rm(temporary, { recursive: true, force: true }); }
  })
  : process.argv[2] === "project-crash-child"
    ? projectCrashChild(...process.argv.slice(3))
    : main();

execution.catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
