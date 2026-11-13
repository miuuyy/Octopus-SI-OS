module.exports = { assertNoExternalTaskTopologyReferences, assertNoRunRecordReferencesEe, assertRunPrivateStateIsOwned, assertRunUsesOnlyOwnedInterfaces, assertTaskWorkerEnvironmentsReleased, assertTaskWorkerPinRemainsTaskLocal, assertTopologyIsPrivateToRun, deleteOctopusOsReadOnlyTask, deleteOctopusOsTask, directJsonDigests, exactStringReferencePaths, filterDescriptorStringList, octopusOsOwnedArtifactTopology, octopusOsOwnedInterfaceIds, octopusOsOwnedToolTopology, octopusOsOwnedWriterRuns, octopusOsOwnedWriterRunsAtRoot, octopusOsTaskGroupArtifactTopology, octopusOsTaskLifecycleGroup, registryControlJsonFiles, removalSnapshot, requirePrivatePublicationDirectory, requireUnchangedRemoval, requiredGoal, runInterfaceReferences, scopeEnvironmentChain, scopedRecordPath, snapshotOwnedInterfaceDirectories, snapshotOwnedInterfaceStateDirectories, snapshotOwnedRegistryDirectories, snapshotReleasedRunWorkerEnvironmentStateDirectories, snapshotRemovalDirectory, snapshotRemovalFile, taskOwnedInterfaceIds, taskLifecycleHelpers, taskLifecycleRetirementRequest, taskTopologyPolicyWrites };

const fs = require("node:fs/promises");
const crypto = require("node:crypto");
const path = require("node:path");
const { blake3 } = require("@noble/hashes/blake3");
const { readOptionalRegularJsonDocument, readRegularJsonDocument, requirePlainDirectory, requireUnchangedJsonDigest, stableJson } = require("./jsonFiles.cjs");
const { listDirectFiles, listWorkerEnvironmentRecordFiles, readDirectoryEntries } = require("./providerDescriptors.cjs");
const { parseOctopusOsSidecar } = require("./sidecars.cjs");
const { readClientTaskArchive } = require("./taskArchive.cjs");
const { taskConnectionWrites, taskCoordinateEvidence } = require("./taskConnections.cjs");
const { arrayValue, contentHashHex, exactObjectKeys, interfaceBindingId, isPlainRecord, isRunRecordJsonPath, objectOrEmpty, requiredArtifactBindingId, requiredDigest, requiredSafeRegistryId, requiredString, safeInterfacePathSegment, stringValue, validWorkerEnvironmentFormation } = require("./values.cjs");

function taskLifecycleHelpers(hooks, record) {
  const required = Object.values(objectOrEmpty(record.interface_state)).some((state) =>
    state?.maintenance_observer !== undefined || state?.collection_catalogue_adoption !== undefined);
  const helpers = hooks.taskLifecycle;
  if (!helpers && !required) return null;
  if (!helpers || ["readMaintenanceTaskTopology", "readTaskCatalogueEvidence", "catalogueProvenanceProjection",
    "assertMaintenanceNoPendingEffects"].some((name) => typeof helpers[name] !== "function")) {
    const error = new Error("task lifecycle ownership reader is unavailable for the accepted maintenance/catalogue contract");
    error.code = "maintenance_lifecycle_unavailable";
    throw error;
  }
  return helpers;
}

async function octopusOsTaskLifecycleGroup(registryRoot, octopusHomeRoot, source, sourceInterfaces, lifecycle, sourceClientInterface) {
  const sourceWriters = await octopusOsOwnedWriterRuns(octopusHomeRoot, source, sourceInterfaces);
  const companions = lifecycle ? await lifecycle.readMaintenanceTaskTopology({
    registryRoot, runRecordsRoot: path.join(octopusHomeRoot, "state", "runs"), sourceRecord: source, sourceClientInterface,
    readWriterRuns: (record, interfaces) => octopusOsOwnedWriterRuns(octopusHomeRoot, record, interfaces),
  }) : null;
  const records = new Map([...sourceWriters.records].map(([id, entry]) => [id, { ...entry, role: "writer", targetRun: source.id }]));
  for (const [id, entry] of companions?.records ?? []) {
    if (id === source.id || records.has(id)) throw new Error(`task '${source.id}' has conflicting companion/writer ownership`);
    records.set(id, entry);
  }
  const exactInterfaces = new Set([...sourceInterfaces, ...companions?.interfaces ?? []]);
  if (companions && [...companions.interfaces].some((id) => sourceInterfaces.has(id))) {
    throw new Error(`task '${source.id}' has overlapping source and companion descriptor owners`);
  }
  const ownedWriters = { records, ids: new Set(records.keys()), writes: [...records.values()]
    .map((entry) => ({ file: entry.file, beforeDigest: entry.digest, next: null })).sort((a, b) => a.file.localeCompare(b.file)) };
  return { sourceInterfaces, exactInterfaces, ownedWriters, companions };
}

function taskLifecycleRetirementRequest(sourcePath, source, group) {
  const rows = [{ ...source, file: sourcePath, role: "source", provider: group.companions.declaration.source_provider_interface },
    ...group.ownedWriters.records.values()];
  return {
    schema_version: 1, source_run: source.document.id,
    interface_ids: [...group.exactInterfaces].sort(),
    records: rows.map((entry) => ({ run: entry.document.id, file: entry.file, expected_digest: entry.digest,
      worker: entry.document.worker, ee: entry.document.ee, role: entry.role,
      provider_interface: typeof entry.provider === "string" ? entry.provider : entry.owner,
      target_run: entry.targetRun ?? entry.document.id })).sort((a, b) => a.run.localeCompare(b.run)),
  };
}

async function octopusOsTaskGroupArtifactTopology(registryRoot, source, group) {
  const targets = [{ document: source, interfaces: group.sourceInterfaces }, ...group.companions?.targets.values() ?? []];
  const ids = new Set(), writes = [];
  for (const target of targets) {
    const owned = await octopusOsOwnedArtifactTopology(registryRoot, target.document, target.interfaces);
    for (const id of owned.ids) {
      if (ids.has(id)) throw new Error(`task '${source.id}' has an artifact owned by two companion targets`);
      ids.add(id);
    }
    writes.push(...owned.writes);
  }
  return { ids, writes };
}

async function deleteOctopusOsTask(
  registryRoot,
  octopusHomeRoot,
  scope,
  request,
  releaseTaskWorkerEnvironments,
  commitClientAuthoring,
  hooks = {},
) {
  if (typeof commitClientAuthoring !== "function") {
    throw new Error("client authoring transaction is unavailable");
  }
  const runId = requiredSafeRegistryId(request?.runId, "runId");
  const runProjection = scope.clientRuns.find((row) => (
    stringValue(row?.run_id) === runId && stringValue(row?.surface_role) === "task"
  ));
  if (!runProjection) {
    const readOnlyProjection = scope.readOnlyRuns.find((row) => stringValue(row?.run_id) === runId);
    if (!readOnlyProjection) throw new Error(`task '${runId}' is absent from the Octopus OS scope`);
    return deleteOctopusOsReadOnlyTask(
      registryRoot,
      octopusHomeRoot,
      readOnlyProjection,
      request,
      commitClientAuthoring,
    );
  }
  let expectedDigest = requiredDigest(request?.expectedDigest, "expectedDigest");
  const runPath = await scopedRecordPath(
    octopusHomeRoot,
    runProjection.record_path,
    path.join("state", "runs"),
    `${runId}.json`,
  );
  const current = await readRegularJsonDocument(runPath, `task run ${runId}`);
  if (current.digest !== expectedDigest || stringValue(runProjection.record_digest) !== expectedDigest) {
    throw new Error(`task run '${runId}' changed before it could be deleted`);
  }
  if (
    stringValue(current.document?.id) !== runId
    || stringValue(current.document?.ee) !== stringValue(runProjection.ee_id)
  ) {
    throw new Error(`task run '${runId}' has drifted topology`);
  }
  const ownerInterface = requiredSafeRegistryId(runProjection.interface_id, "task client interface");
  if (readClientTaskArchive(current.document, ownerInterface)) throw new Error(`task '${runId}' is archived; restore it before deleting`);
  const sourceInterfaces = await taskOwnedInterfaceIds(registryRoot, current.document, ownerInterface, runId);
  assertRunPrivateStateIsOwned(current.document, sourceInterfaces, runId);
  const lifecycle = taskLifecycleHelpers(hooks, current.document);
  const group = await octopusOsTaskLifecycleGroup(
    registryRoot, octopusHomeRoot, current.document, sourceInterfaces, lifecycle, ownerInterface,
  );
  const { exactInterfaces, ownedWriters, companions } = group;
  const connectionWrites = await taskConnectionWrites(octopusHomeRoot, scope,
    new Set([runId, ...ownedWriters.ids]), exactInterfaces, hooks);
  const connectionDocuments = new Map(connectionWrites.map(write => [write.file, write]));
  let retirementSelection = companions ? taskLifecycleRetirementRequest(runPath, current, group) : null;
  if (companions) {
    lifecycle.assertMaintenanceNoPendingEffects({ targets: new Map([
      [runId, { document: current.document, provider: companions.declaration.source_provider_interface }],
      ...companions.targets,
    ]) });
  }
  await assertTopologyIsPrivateToRun(octopusHomeRoot, runId, exactInterfaces, new Set(), ownedWriters.ids, connectionDocuments);

  const registryHomeRoot = await fs.realpath(octopusHomeRoot);
  const registryHome = await fs.realpath(registryRoot);
  if (path.dirname(registryHome) !== registryHomeRoot || path.basename(registryHome) !== "registry") {
    throw new Error("Octopus OS registry root is outside the active Octopus home");
  }
  const interfaceRoot = await fs.realpath(path.join(registryHome, "interface"));
  const interfaceDirectories = await snapshotOwnedRegistryDirectories(
    interfaceRoot,
    exactInterfaces,
    "task interface",
  );
  const ownedTools = await octopusOsOwnedToolTopology(registryHome, exactInterfaces, companions);
  const ownedArtifacts = await octopusOsTaskGroupArtifactTopology(registryHome, current.document, group);
  const coordinates = await taskCoordinateEvidence(registryHome,
    [{ document: current.document, interfaces: sourceInterfaces }, ...companions?.targets.values() ?? []]);
  const catalogueEvidence = lifecycle ? await lifecycle.readTaskCatalogueEvidence({
    registryRoot: registryHome,
    records: [current.document, ...[...companions?.targets.values() ?? []].map((entry) => entry.document)],
    canonicalContentHash: (value) => Buffer.from(blake3(Buffer.from(stableJson(value)))).toString("hex"),
  }) : new Map();
  await assertTopologyIsPrivateToRun(octopusHomeRoot, runId, exactInterfaces, ownedArtifacts.ids, ownedWriters.ids, connectionDocuments);
  const policyWrites = await taskTopologyPolicyWrites(registryHome, exactInterfaces, ownedTools.ids);
  const plannedDocuments = new Map([...policyWrites, ...ownedArtifacts.writes].map((write) => [write.file, write.next]));
  for (const [file, entry] of coordinates) plannedDocuments.set(file, entry.projection);
  await assertNoExternalTaskTopologyReferences(
    registryHome,
    new Set([
      ...interfaceDirectories.map((entry) => entry.path),
      ...ownedTools.directories.map((entry) => entry.path),
    ]),
    plannedDocuments,
    new Set([runId, ...ownedWriters.ids, ...exactInterfaces, ...ownedTools.ids, ...ownedArtifacts.ids]),
    lifecycle,
  );

  let releasedWorkerEnvironmentIds = [];
  if (typeof releaseTaskWorkerEnvironments === "function") {
    const released = await releaseTaskWorkerEnvironments({
      runId,
      expectedDigest,
      timeoutMs: request?.timeoutMs,
    });
    if (stringValue(released?.runId) !== runId) {
      throw new Error(`task '${runId}' release hook returned another run identity`);
    }
    expectedDigest = requiredDigest(released?.expectedDigest, "released task digest");
    releasedWorkerEnvironmentIds = arrayValue(released?.releasedWorkerEnvironmentIds)
      .map((id) => requiredSafeRegistryId(id, "released worker environment id"));
    if (new Set(releasedWorkerEnvironmentIds).size !== releasedWorkerEnvironmentIds.length) {
      throw new Error(`task '${runId}' release hook repeated a worker environment`);
    }
    const releasedRun = await readRegularJsonDocument(runPath, `released task run ${runId}`);
    if (
      releasedRun.digest !== expectedDigest
      || stringValue(releasedRun.document?.id) !== runId
      || stringValue(releasedRun.document?.ee) !== stringValue(runProjection.ee_id)
    ) {
      throw new Error(`task run '${runId}' changed outside its exact release cycle`);
    }
    const releasedInterfaces = await taskOwnedInterfaceIds(registryRoot, releasedRun.document, ownerInterface, runId);
    if (stableJson([...releasedInterfaces].sort()) !== stableJson([...sourceInterfaces].sort())) {
      throw new Error(`task '${runId}' interface ownership changed during release`);
    }
    assertRunPrivateStateIsOwned(releasedRun.document, releasedInterfaces, runId);
    await assertTopologyIsPrivateToRun(octopusHomeRoot, runId, exactInterfaces, ownedArtifacts.ids, ownedWriters.ids, connectionDocuments);
    const releasedGroup = await octopusOsTaskLifecycleGroup(
      registryRoot, octopusHomeRoot, releasedRun.document, sourceInterfaces, lifecycle, ownerInterface,
    );
    if (stableJson([...releasedGroup.exactInterfaces].sort()) !== stableJson([...exactInterfaces].sort())
      || stableJson(releasedGroup.ownedWriters.writes) !== stableJson(ownedWriters.writes)) {
      throw new Error(`task '${runId}' writer ownership changed during release`);
    }
    const releasedArtifacts = await octopusOsTaskGroupArtifactTopology(registryHome, releasedRun.document, releasedGroup);
    if (stableJson(releasedArtifacts.writes) !== stableJson(ownedArtifacts.writes)) {
      throw new Error(`task '${runId}' artifact ownership changed during release`);
    }
    await assertNoExternalTaskTopologyReferences(
      registryHome,
      new Set([
        ...interfaceDirectories.map((entry) => entry.path),
        ...ownedTools.directories.map((entry) => entry.path),
      ]),
      plannedDocuments,
      new Set([runId, ...ownedWriters.ids, ...exactInterfaces, ...ownedTools.ids, ...ownedArtifacts.ids]),
      lifecycle,
    );
    if (companions) retirementSelection = taskLifecycleRetirementRequest(runPath, releasedRun, releasedGroup);
  }

  const runtimeStateDirectories = await snapshotOwnedInterfaceStateDirectories(
    registryHomeRoot,
    exactInterfaces,
  );
  await assertTaskWorkerEnvironmentsReleased(registryHomeRoot, exactInterfaces, runId, ownedWriters.records, current.document);
  for (const [file, snapshot] of catalogueEvidence) {
    await requireUnchangedJsonDigest(file, snapshot.digest, "accepted catalogue evidence");
  }
  for (const [file, snapshot] of coordinates) await requireUnchangedJsonDigest(file, snapshot.digest, "task authoring coordinates");
  for (const removal of [...interfaceDirectories, ...ownedTools.directories]) await requireUnchangedRemoval(removal);
  for (const write of ownedWriters.writes) await snapshotRemovalFile(write.file, write.beforeDigest, "task writer Run");
  await snapshotRemovalFile(runPath, expectedDigest, `task run ${runId}`);
  const committed = await commitClientAuthoring({
    label: "delete Octopus OS task",
    writes: [...connectionWrites, ...policyWrites, ...ownedArtifacts.writes, ...ownedWriters.writes, { file: runPath, beforeDigest: expectedDigest, next: null }],
    removals: [
      ...interfaceDirectories,
      ...ownedTools.directories,
      ...runtimeStateDirectories,
    ],
    publishBarrierPath: runPath,
    ...(retirementSelection ? { retirement: retirementSelection } : {}),
  });
  return {
    action: "delete_task",
    runId,
    removedRuns: [runId, ...ownedWriters.ids].sort(),
    removedInterfaces: [...exactInterfaces].sort(),
    removedTools: [...ownedTools.ids].sort(),
    removedArtifacts: [...ownedArtifacts.ids].sort(),
    releasedWorkerEnvironmentIds,
    cleanupWarnings: arrayValue(committed?.cleanupWarnings),
  };
}

async function deleteOctopusOsReadOnlyTask(
  registryRoot,
  octopusHomeRoot,
  runProjection,
  request,
  commitClientAuthoring,
) {
  const runId = requiredSafeRegistryId(request?.runId, "runId");
  const expectedDigest = requiredDigest(request?.expectedDigest, "expectedDigest");
  const runPath = await scopedRecordPath(
    octopusHomeRoot,
    runProjection.record_path,
    path.join("state", "runs"),
    `${runId}.json`,
  );
  const current = await readRegularJsonDocument(runPath, `read-only task run ${runId}`);
  if (current.digest !== expectedDigest || stringValue(runProjection.record_digest) !== expectedDigest) {
    throw new Error(`read-only task run '${runId}' changed before it could be deleted`);
  }
  if (
    stringValue(current.document?.id) !== runId
    || stringValue(current.document?.ee) !== stringValue(runProjection.ee_id)
  ) {
    throw new Error(`read-only task run '${runId}' has drifted topology`);
  }
  await assertTopologyIsPrivateToRun(octopusHomeRoot, runId, new Set());
  const registryHomeRoot = await fs.realpath(octopusHomeRoot);
  const registryHome = await fs.realpath(registryRoot);
  if (path.dirname(registryHome) !== registryHomeRoot || path.basename(registryHome) !== "registry") {
    throw new Error("Octopus OS registry root is outside the active Octopus home");
  }
  await assertNoExternalTaskTopologyReferences(
    registryHome,
    new Set(),
    new Map(),
    new Set([runId]),
  );
  const workerEnvironmentState = await snapshotReleasedRunWorkerEnvironmentStateDirectories(
    registryHomeRoot,
    current.document,
    runId,
  );
  await snapshotRemovalFile(runPath, expectedDigest, `read-only task run ${runId}`);
  const committed = await commitClientAuthoring({
    label: "delete read-only Octopus OS task",
    writes: [{ file: runPath, beforeDigest: expectedDigest, next: null }],
    removals: workerEnvironmentState.directories,
    publishBarrierPath: runPath,
  });
  return {
    action: "delete_task",
    runId,
    removedInterfaces: [],
    removedTools: [],
    releasedWorkerEnvironmentIds: workerEnvironmentState.ids,
    cleanupWarnings: arrayValue(committed?.cleanupWarnings),
  };
}

function requiredGoal(value) {
  const goal = requiredString(value, "goal");
  if (goal.length > 512 || /[\u0000-\u001f\u007f]/.test(goal)) {
    throw new Error("goal must be at most 512 visible characters");
  }
  return goal;
}

async function scopedRecordPath(octopusHomeRoot, projectedPath, relativeRoot, fallbackName) {
  const root = await fs.realpath(path.join(octopusHomeRoot, relativeRoot));
  const candidate = stringValue(projectedPath) || path.join(root, fallbackName);
  const resolvedParent = await fs.realpath(path.dirname(candidate));
  if (resolvedParent !== root || path.basename(candidate) !== fallbackName) {
    throw new Error(`projected record path is outside '${root}'`);
  }
  return path.join(resolvedParent, fallbackName);
}

function scopeEnvironmentChain(eeId, byId, rootEeId) {
  const chain = [];
  const seen = new Set();
  let cursor = eeId;
  while (cursor) {
    if (seen.has(cursor)) throw new Error(`EE hierarchy cycles at '${cursor}'`);
    seen.add(cursor);
    const environment = byId.get(cursor);
    if (!environment) throw new Error(`EE '${cursor}' is absent from the Octopus OS scope`);
    chain.push(environment);
    if (cursor === rootEeId) return chain.reverse();
    cursor = requiredSafeRegistryId(environment.parent_ee_id, `parent of EE ${cursor}`);
  }
  throw new Error(`EE '${eeId}' is not below the Octopus OS root`);
}

async function taskOwnedInterfaceIds(registryRoot, run, ownerInterface, runId) {
  const ids = await octopusOsOwnedInterfaceIds(registryRoot, ownerInterface);
  const providerId = requiredSafeRegistryId(run.interface_state[ownerInterface].provider_runtime_interface, "task provider interface");
  if (!ids.has(providerId)) throw new Error(`task '${runId}' has a provider outside its exact client topology`);
  const interfaceRoot = await fs.realpath(path.join(registryRoot, "interface"));
  const readInterface = async (id) => {
    await requirePlainDirectory(path.join(interfaceRoot, id), `task interface ${id}`);
    const { document } = await readRegularJsonDocument(path.join(interfaceRoot, id, "interface.json"), `task interface ${id}`);
    if (document?.id !== id) throw new Error(`task interface '${id}' has drifted descriptor identity`);
    return document;
  };
  const provider = await readInterface(providerId);
  const selection = provider.interface_driver_config?.private_config?.input_intake;
  if (selection === undefined) return ids;
  const label = `task '${runId}' input intake`;
  exactObjectKeys(selection, ["template_interface_id", "interface_id"], ["template_interface_id", "interface_id"], label);
  requiredSafeRegistryId(selection.template_interface_id, `${label} template`);
  const intakeId = requiredSafeRegistryId(selection.interface_id, `${label} interface`);
  const intake = await readInterface(intakeId);
  const declaration = intake.interface_driver_config?.private_config?.input_intake;
  exactObjectKeys(declaration,
    ["schema_version", "run", "provider_interface", "writer", "writer_required_verified_artifacts", "profile", "provider_profile", "input_label", "artifact_bindings", "notification_action"],
    ["schema_version", "run", "provider_interface", "writer", "writer_required_verified_artifacts", "profile", "provider_profile", "input_label", "artifact_bindings"], label);
  if (declaration.notification_action !== undefined && !["queue", "followup"].includes(declaration.notification_action)) {
    throw new Error(`${label} has an invalid notification action`);
  }
  if (intake.interface_type !== "system" || intake.role !== "service" || intake.interface_driver_ref !== "checked_input_intake"
    || declaration.schema_version !== 1 || declaration.run !== runId || declaration.provider_interface !== providerId
    || !isPlainRecord(run.interface_state[intakeId])
    || stableJson(run.interface_state[intakeId].input_intake) !== stableJson(declaration)) {
    throw new Error(`${label} does not match its exact run/provider declaration`);
  }
  const writer = declaration.writer;
  exactObjectKeys(writer, ["owner", "source", "door", "tool", "call_label", "result_label"],
    ["owner", "source", "door", "tool", "call_label", "result_label"], `${label} writer`);
  const doorId = requiredSafeRegistryId(writer.door, `${label} writer door`);
  const toolId = requiredString(writer.tool, `${label} writer tool`);
  if (!safeInterfacePathSegment(toolId)) throw new Error(`${label} writer tool is not a safe path segment`);
  if (writer.owner !== intakeId || writer.source !== intakeId || new Set([ownerInterface, providerId, intakeId, doorId]).size !== 4) {
    throw new Error(`${label} writer has inconsistent exact interfaces`);
  }
  requiredString(declaration.input_label, `${label} input label`);
  requiredString(writer.call_label, `${label} call label`);
  requiredString(writer.result_label, `${label} result label`);
  for (const profile of [declaration.profile, declaration.provider_profile]) {
    exactObjectKeys(profile, ["file_write_tool", "file_write_label", "file_write_result_label"],
      ["file_write_tool", "file_write_label", "file_write_result_label"], `${label} profile`);
  }
  if (declaration.profile.file_write_tool !== toolId || declaration.profile.file_write_label !== writer.call_label
    || declaration.profile.file_write_result_label !== writer.result_label || writer.call_label === declaration.input_label
    || !Array.isArray(declaration.artifact_bindings) || !Array.isArray(declaration.writer_required_verified_artifacts)
    || declaration.writer_required_verified_artifacts.length !== 1
    || declaration.writer_required_verified_artifacts[0]?.artifact_label !== writer.call_label) {
    throw new Error(`${label} has inconsistent writer contract`);
  }
  await readInterface(doorId);
  const toolRoot = await fs.realpath(path.join(registryRoot, "tool"));
  await requirePlainDirectory(path.join(toolRoot, toolId), `${label} tool`);
  const tool = await readRegularJsonDocument(path.join(toolRoot, toolId, "tool.json"), `${label} tool`);
  if (tool.document?.id !== toolId || tool.document?.interface !== doorId) throw new Error(`${label} tool does not belong to its exact writer door`);
  const workerId = requiredSafeRegistryId(run.worker, "task worker interface");
  if (!ids.has(workerId)) throw new Error(`${label} worker is outside its exact client topology`);
  const policy = await readRegularJsonDocument(path.join(interfaceRoot, workerId, "policy.json"), `${label} worker ceiling`);
  if (policy.document?.id !== workerId || !Array.isArray(policy.document.ceiling?.interfaces)
    || !Array.isArray(policy.document.ceiling?.tools)
    || [providerId, intakeId, doorId].some((id) => !policy.document.ceiling.interfaces.includes(id))
    || !policy.document.ceiling.tools.includes(toolId)) {
    throw new Error(`${label} writer is outside its exact worker ceiling`);
  }
  const owned = run.interface_state[intakeId].owned_checkpoint_ids;
  if (!Array.isArray(owned) || owned.length === 0 || new Set(owned).size !== owned.length) throw new Error(`${label} has malformed checkpoint ownership`);
  for (const [interfaceId, state] of Object.entries(run.interface_state)) {
    if (!isPlainRecord(state)) throw new Error(`${label} has malformed private state for '${interfaceId}'`);
    for (const field of ["owned_checkpoint_ids", "frontier_checkpoint_ids", "cycle_checkpoint_ids"]) {
      if (state[field] !== undefined && (!Array.isArray(state[field]) || state[field].some((id) => typeof id !== "string"))) {
        throw new Error(`${label} has malformed ${field} for '${interfaceId}'`);
      }
    }
  }
  for (const id of owned) {
    requiredSafeRegistryId(id, `${label} owned checkpoint`);
    const rows = run.checkpoints.filter((checkpoint) => checkpoint.id === id);
    if (rows.length !== 1 || rows[0].run !== runId
      || ![intakeId, doorId].includes(interfaceBindingId(rows[0].interface_binding))
      || !Array.isArray(rows[0].outbound_interface_bindings) || rows[0].outbound_interface_bindings.length !== 1
      || rows[0].outbound_interface_bindings.some((binding) => ![intakeId, doorId, providerId].includes(interfaceBindingId(binding)))
      || Object.entries(run.interface_state).some(([interfaceId, state]) => interfaceId !== intakeId
        && ["owned_checkpoint_ids", "frontier_checkpoint_ids", "cycle_checkpoint_ids"].some((field) => arrayValue(state?.[field]).includes(id)))) {
      throw new Error(`${label} checkpoint '${id}' is outside its exact ownership`);
    }
  }
  ids.add(intakeId);
  ids.add(doorId);
  return ids;
}

async function octopusOsOwnedInterfaceIds(registryRoot, ownerInterface) {
  const interfaceRoot = await fs.realpath(path.join(registryRoot, "interface"));
  const entries = await fs.readdir(interfaceRoot, { withFileTypes: true });
  const ids = new Set();
  for (const entry of entries) {
    if (!entry.isDirectory() || !safeInterfacePathSegment(entry.name)) continue;
    const sidecarPath = path.join(interfaceRoot, entry.name, "octopus_os.json");
    const sidecar = await readOptionalRegularJsonDocument(sidecarPath, `Octopus OS sidecar ${entry.name}`);
    if (!sidecar) continue;
    const parsed = parseOctopusOsSidecar(sidecar.document, entry.name, sidecarPath);
    if (stringValue(parsed.owner_interface) === ownerInterface) ids.add(entry.name);
  }
  if (!ids.has(ownerInterface)) throw new Error(`task client interface '${ownerInterface}' has no exact topology sidecar`);
  return ids;
}

function assertRunUsesOnlyOwnedInterfaces(run, exactInterfaces, runId) {
  const used = runInterfaceReferences(run);
  for (const checkpoint of arrayValue(run?.checkpoints)) {
    if (stringValue(checkpoint?.run) !== runId) {
      throw new Error(`task run '${runId}' has a checkpoint owned by another run`);
    }
  }
  for (const interfaceId of used) {
    if (!interfaceId || !exactInterfaces.has(interfaceId)) {
      throw new Error(`task run '${runId}' binds interface '${interfaceId || "<empty>"}' outside its exact client topology`);
    }
  }
}

function assertRunPrivateStateIsOwned(run, exactInterfaces, runId) {
  const worker = stringValue(run?.worker);
  if (!worker || !exactInterfaces.has(worker)) {
    throw new Error(`task run '${runId}' wears worker '${worker || "<empty>"}' outside its exact client topology`);
  }
  for (const interfaceId of Object.keys(objectOrEmpty(run?.interface_state))) {
    if (!exactInterfaces.has(interfaceId)) {
      throw new Error(`task run '${runId}' stores private state for interface '${interfaceId}' outside its exact client topology`);
    }
  }
  for (const checkpoint of arrayValue(run?.checkpoints)) {
    if (stringValue(checkpoint?.run) !== runId) {
      throw new Error(`task run '${runId}' has a checkpoint owned by another run`);
    }
  }
}

async function assertTaskWorkerPinRemainsTaskLocal(
  octopusHomeRoot,
  workerInterface,
  ownerInterface,
  run,
  exactInterfaces,
) {
  const writers = await octopusOsOwnedWriterRuns(octopusHomeRoot, run, exactInterfaces);
  const runRoot = await fs.realpath(path.join(octopusHomeRoot, "state", "runs"));
  for (const file of await listDirectFiles(runRoot, isRunRecordJsonPath)) {
    let candidate;
    try {
      candidate = await readRegularJsonDocument(file, `run record ${file}`);
    } catch {
      continue;
    }
    if (stringValue(candidate.document?.worker) !== workerInterface) continue;
    const writer = writers.records.get(candidate.document?.id);
    if (writer && candidate.digest === writer.digest && file === writer.file) continue;
    if (!runInterfaceReferences(candidate.document).has(ownerInterface)) {
      throw new Error(
        `task worker '${workerInterface}' is also worn by run '${stringValue(candidate.document?.id)}' outside exact task interface '${ownerInterface}'`,
      );
    }
  }
}

async function octopusOsOwnedWriterRuns(octopusHomeRoot, run, exactInterfaces) {
  const runRoot = await fs.realpath(path.join(octopusHomeRoot, "state", "runs"));
  return octopusOsOwnedWriterRunsAtRoot(runRoot, run, exactInterfaces);
}

async function octopusOsOwnedWriterRunsAtRoot(runRoot, run, exactInterfaces) {
  const ids = new Set(), writes = [], records = new Map();
  for (const [owner, state] of Object.entries(objectOrEmpty(run.interface_state))) {
    const provider = state?.run_authoring_binding;
    const binding = provider?.writer_run ?? state?.writer_run;
    if (binding === undefined) continue;
    const label = `task '${run.id}' writer for '${owner}'`;
    exactObjectKeys(binding, ["run", "initial_record", "initial_record_format", "role_artifact_bindings"], ["run", "initial_record", "role_artifact_bindings"], label);
    if (Object.hasOwn(binding, "initial_record_format") && !["pretty", "compact"].includes(binding.initial_record_format)) {
      throw new Error(`${label} has invalid initial record format`);
    }
    const id = requiredSafeRegistryId(binding.run, `${label} Run`);
    if (id === run.id || ids.has(id) || !exactInterfaces.has(owner)
      || (provider && state.writer_run !== undefined) || !Array.isArray(binding.role_artifact_bindings)) {
      throw new Error(`${label} has conflicting ownership`);
    }
    const initial = binding.initial_record;
    exactObjectKeys(initial, ["id", "ee", "worker", "trace", "status", "interface_state", "checkpoints", "active_checkpoints"],
      ["id", "ee", "worker", "trace", "status", "interface_state", "checkpoints", "active_checkpoints"], label);
    if (initial.id !== id || initial.worker !== run.worker
      || initial.trace !== `trace:${id}` || initial.status !== "idle"
      || !isPlainRecord(initial.interface_state) || stableJson(Object.keys(initial.interface_state)) !== stableJson([owner])) {
      throw new Error(`${label} changed its exact identity, worker or EE`);
    }
    exactObjectKeys(initial.interface_state[owner], ["file_writer_target"], ["file_writer_target"], label);
    const target = initial.interface_state[owner].file_writer_target;
    exactObjectKeys(target, ["run", "provider", "source", "door", "generation"], ["run", "provider", "source", "door", "generation"], label);
    if (target.run !== run.id || target.provider !== owner || target.generation !== 0
      || !exactInterfaces.has(target.source) || !exactInterfaces.has(target.door) || target.source === target.door) {
      throw new Error(`${label} changed its exact target or private interfaces`);
    }
    let templates;
    if (provider) {
      if (provider.run !== run.id || provider.worker !== run.worker || provider.provider_interface !== owner
        || provider.authoring_source_interface !== target.source || !Array.isArray(provider.checkpoints)) {
        throw new Error(`${label} differs from its provider declaration`);
      }
      templates = ["authoring_call", "authoring_result"].map((role) => {
        const rows = provider.checkpoints.filter((row) => row.role === role);
        if (rows.length !== 1) throw new Error(`${label} has ambiguous authoring roles`);
        return rows[0];
      });
    } else {
      const writer = state.input_intake?.writer;
      if (!writer || writer.owner !== owner || writer.source !== target.source || writer.door !== target.door
        || state.input_intake.run !== run.id) throw new Error(`${label} lacks its exact intake declaration`);
    }
    if (!Array.isArray(initial.checkpoints) || initial.checkpoints.length !== 2 || !Array.isArray(initial.active_checkpoints)) {
      throw new Error(`${label} needs one exact call/result pair`);
    }
    const checkpoints = new Set();
    for (const [source, destination] of [[target.source, target.door], [target.door, owner]]) {
      const rows = initial.checkpoints.filter((cp) => interfaceBindingId(cp.interface_binding) === source
        && Array.isArray(cp.outbound_interface_bindings) && cp.outbound_interface_bindings.length === 1
        && interfaceBindingId(cp.outbound_interface_bindings[0]) === destination);
      if (rows.length !== 1) throw new Error(`${label} changed its exact call/result edge`);
      if (arrayValue(run.checkpoints).some((cp) => interfaceBindingId(cp.interface_binding) === source
        && arrayValue(cp.outbound_interface_bindings).some((binding) => interfaceBindingId(binding) === destination))) {
        throw new Error(`${label} duplicates its writer edge in the target Run`);
      }
      const cp = rows[0], checkpointId = requiredSafeRegistryId(cp.id, `${label} checkpoint`);
      if (checkpoints.has(checkpointId) || cp.run !== id || cp.trace !== `trace:${id}:${checkpointId}`
        || !Number.isSafeInteger(cp.epoch) || cp.epoch < 0
        || !Array.isArray(cp.artifact_bindings) || !Array.isArray(cp.required_verified_artifacts)) {
        throw new Error(`${label} has malformed checkpoint ownership`);
      }
      if (templates) {
        const template = templates[source === target.source ? 0 : 1];
        for (const key of ["interface_binding", "outbound_interface_bindings", "required_verified_artifacts"]) {
          if (stableJson(cp[key]) !== stableJson(template[key])) throw new Error(`${label} changed its declared ${key}`);
        }
      }
      checkpoints.add(checkpointId);
    }
    if (stableJson([...checkpoints].sort()) !== stableJson([...initial.active_checkpoints].sort())) {
      throw new Error(`${label} has an unrelated active checkpoint`);
    }
    const file = path.join(runRoot, `${id}.json`);
    const current = await readRegularJsonDocument(file, label);
    const generation = current.document?.interface_state?.[owner]?.file_writer_target?.generation;
    if (!Number.isSafeInteger(generation) || generation < 0) throw new Error(`${label} has invalid generation`);
    const expected = structuredClone(initial);
    if (generation > 0) {
      expected.interface_state[owner].file_writer_target.generation = generation;
      for (const cp of expected.checkpoints) {
        cp.id = `${cp.id}_${generation}`; cp.trace = `trace:${id}:${cp.id}`;
      }
      expected.active_checkpoints = expected.checkpoints.map((cp) => cp.id);
    }
    if (stableJson(current.document) !== stableJson(expected)) throw new Error(`${label} drifted from its declared generation`);
    ids.add(id);
    records.set(id, { ...current, file, owner, provider: Boolean(provider) });
    writes.push({ file, beforeDigest: current.digest, next: null });
  }
  return { ids, writes, records };
}

async function assertTopologyIsPrivateToRun(octopusHomeRoot, runId, exactInterfaces, artifactIds = new Set(), ownedRunIds = new Set(), plannedRecords = new Map()) {
  const runRoot = await fs.realpath(path.join(octopusHomeRoot, "state", "runs"));
  for (const file of await listDirectFiles(runRoot, isRunRecordJsonPath)) {
    if (path.basename(file) === `${runId}.json` || ownedRunIds.has(path.basename(file, ".json"))) continue;
    const candidate = await readRegularJsonDocument(file, `run record ${file}`);
    const planned = plannedRecords.get(file);
    if (planned) {
      if (candidate.digest !== planned.beforeDigest) throw new Error("Connected task changed before deletion");
      candidate.document = planned.next;
    }
    const used = runInterfaceReferences(candidate.document);
    for (const interfaceId of exactInterfaces) {
      if (used.has(interfaceId)) throw new Error(`exact interface '${interfaceId}' is also bound by run '${stringValue(candidate.document?.id)}'`);
    }
    const runReferences = exactStringReferencePaths(candidate.document, new Set([runId, ...ownedRunIds, ...artifactIds]));
    if (runReferences.length) {
      throw new Error(`task run '${runId}' is referenced by run '${stringValue(candidate.document?.id)}' at ${runReferences[0]}`);
    }
  }
}

function runInterfaceReferences(run) {
  const used = new Set([stringValue(run?.worker)]);
  for (const interfaceId of Object.keys(objectOrEmpty(run?.interface_state))) used.add(interfaceId);
  for (const checkpoint of arrayValue(run?.checkpoints)) {
    const inbound = interfaceBindingId(checkpoint?.interface_binding);
    if (inbound) used.add(inbound);
    for (const outbound of arrayValue(checkpoint?.outbound_interface_bindings).map(interfaceBindingId).filter(Boolean)) used.add(outbound);
  }
  return used;
}

async function snapshotOwnedRegistryDirectories(root, ids, label) {
  const directories = [];
  for (const id of [...ids].sort()) {
    if (!safeInterfacePathSegment(id)) throw new Error(`${label} id '${id}' is not a safe path segment`);
    const directory = path.join(root, id);
    const descriptor = await readRegularJsonDocument(path.join(directory, "interface.json"), `${label} ${id}`);
    if (stringValue(descriptor.document?.id ?? descriptor.document?.rule_id) !== id) {
      throw new Error(`${label} '${id}' has drifted descriptor identity`);
    }
    directories.push(await snapshotRemovalDirectory(directory, `${label} ${id}`, true));
  }
  return directories;
}

async function octopusOsOwnedToolTopology(registryRoot, exactInterfaces, companions = null) {
  const toolRoot = await fs.realpath(path.join(registryRoot, "tool"));
  const entries = await fs.readdir(toolRoot, { withFileTypes: true });
  const ids = new Set();
  const directories = [];
  for (const entry of entries) {
    if (entry.name === ".octopus-publication") { await requirePrivatePublicationDirectory(path.join(toolRoot, entry.name)); continue; }
    if (!entry.isDirectory() || !safeInterfacePathSegment(entry.name)) continue;
    const directory = path.join(toolRoot, entry.name);
    const descriptor = await readRegularJsonDocument(path.join(directory, "tool.json"), `tool descriptor ${entry.name}`);
    if (stringValue(descriptor.document?.id) !== entry.name) {
      throw new Error(`tool descriptor '${entry.name}' has drifted identity`);
    }
    if (!exactInterfaces.has(stringValue(descriptor.document?.interface))) continue;
    if (companions?.interfaces.has(descriptor.document.interface) && !companions.tools.has(entry.name)) {
      throw new Error(`tool '${entry.name}' reaches companion topology without declared ownership`);
    }
    ids.add(entry.name);
    directories.push(await snapshotRemovalDirectory(directory, `task tool ${entry.name}`, true));
  }
  return { ids, directories };
}

async function octopusOsOwnedArtifactTopology(registryRoot, run, exactInterfaces) {
  const ids = new Set(), writes = [], documents = new Map();
  const artifactRoot = path.join(registryRoot, "artifact");
  const reference = (value) => {
    exactObjectKeys(value, ["id", "content"], ["id", "content"], "task artifact reference");
    const id = requiredArtifactBindingId(value.id, "task artifact id");
    if ([".", ".."].includes(id) || id.includes("/") || id.includes("\\")
      || !Array.isArray(value.content) || value.content.length !== 32
      || value.content.some((byte) => !Number.isInteger(byte) || byte < 0 || byte > 255)) {
      throw new Error(`task artifact '${id}' has invalid identity or content pin`);
    }
    return id;
  };
  const readOwned = async (ref) => {
    const id = reference(ref);
    if (await fs.realpath(artifactRoot) !== artifactRoot) throw new Error("task artifact registry must not be a symbolic link");
    const directory = path.join(artifactRoot, id);
    await requirePlainDirectory(directory, `task artifact ${id}`);
    const file = path.join(directory, "artifact.json");
    const descriptor = await readRegularJsonDocument(file, `task artifact ${id}`);
    const hash = Buffer.from(blake3(Buffer.from(stableJson(descriptor.document)))).toString("hex");
    if (descriptor.document?.id !== id || descriptor.document?.run !== run.id || hash !== contentHashHex(ref.content)) {
      throw new Error(`task artifact '${id}' has drifted identity, ownership or content`);
    }
    if (documents.has(id) && documents.get(id).digest !== descriptor.digest) throw new Error(`task artifact '${id}' changed during ownership validation`);
    if (!ids.has(id)) {
      ids.add(id);
      documents.set(id, descriptor);
      writes.push({ file, beforeDigest: descriptor.digest, next: null });
    }
    return descriptor.document;
  };
  for (const interfaceId of [...exactInterfaces].sort()) {
    const sidecar = await readOptionalRegularJsonDocument(
      path.join(registryRoot, "interface", interfaceId, "code_mode.json"), `task Code Mode ${interfaceId}`,
    );
    if (!sidecar || sidecar.document?.context?.mode !== "persistent") continue;
    const preludeRef = sidecar.document.context.prelude_artifact;
    const topology = await readOwned(sidecar.document.handoff?.topology_artifact);
    const prelude = await readOwned(preludeRef);
    if (topology.owner_interface !== interfaceId || topology.worker !== run.worker
      || prelude.owner_interface !== interfaceId || stableJson(topology.prelude_artifact) !== stableJson(preludeRef)) {
      throw new Error(`task Code Mode '${interfaceId}' has inconsistent artifact ownership`);
    }
    const sources = run.interface_state?.[interfaceId]?.prelude_sources;
    if (!Array.isArray(sources) || sources.length === 0) throw new Error(`task Code Mode '${interfaceId}' has no source ownership evidence`);
    const sourceIds = new Set();
    for (const source of sources) {
      exactObjectKeys(source, ["source", "snapshot", "source_sha256"], ["source", "snapshot", "source_sha256"], "task prelude source");
      const sourceId = reference(source.source);
      const snapshotId = reference(source.snapshot);
      if (sourceId === snapshotId || sourceIds.has(sourceId)) throw new Error("task prelude source ownership is ambiguous");
      sourceIds.add(sourceId);
      const snapshot = await readOwned(source.snapshot);
      if (stableJson(snapshot.source) !== stableJson(source.source) || snapshot.source_sha256 !== source.source_sha256
        || typeof snapshot.source_content !== "string"
        || crypto.createHash("sha256").update(snapshot.source_content).digest("hex") !== source.source_sha256) {
        throw new Error(`task prelude snapshot '${snapshotId}' differs from its source evidence`);
      }
      const original = JSON.parse(snapshot.source_content);
      if (original?.id !== sourceId || Buffer.from(blake3(Buffer.from(stableJson(original)))).toString("hex") !== contentHashHex(source.source.content)) {
        throw new Error(`task prelude snapshot '${snapshotId}' has drifted source identity or content`);
      }
    }
  }
  return { ids, writes };
}

async function taskTopologyPolicyWrites(registryRoot, exactInterfaces, ownedTools) {
  const writes = [];
  const interfaceRoot = await fs.realpath(path.join(registryRoot, "interface"));
  for (const entry of await fs.readdir(interfaceRoot, { withFileTypes: true })) {
    if (entry.name === ".octopus-publication") { await requirePrivatePublicationDirectory(path.join(interfaceRoot, entry.name)); continue; }
    if (!entry.isDirectory() || exactInterfaces.has(entry.name)) continue;
    const file = path.join(interfaceRoot, entry.name, "policy.json");
    const policy = await readOptionalRegularJsonDocument(file, `worker policy ${entry.name}`);
    if (!policy) continue;
    if (policy.document.id !== entry.name || !isPlainRecord(policy.document.ceiling)) {
      throw new Error(`worker policy '${entry.name}' has drifted identity or ceiling`);
    }
    const next = structuredClone(policy.document);
    filterDescriptorStringList(next.ceiling, "interfaces", exactInterfaces, `worker ${entry.name}`);
    filterDescriptorStringList(next.ceiling, "tools", ownedTools, `worker ${entry.name}`);
    filterDescriptorStringList(next.ceiling, "hard_blocked_tools", ownedTools, `worker ${entry.name}`);
    if (stableJson(next) !== stableJson(policy.document)) writes.push({ file, beforeDigest: policy.digest, next });
  }
  const eeTemplateRoot = await fs.realpath(path.join(registryRoot, "ee_template"));
  for (const entry of await fs.readdir(eeTemplateRoot, { withFileTypes: true })) {
    if (entry.name === ".octopus-publication") { await requirePrivatePublicationDirectory(path.join(eeTemplateRoot, entry.name)); continue; }
    if (!entry.isDirectory() || !safeInterfacePathSegment(entry.name)) continue;
    const file = path.join(eeTemplateRoot, entry.name, "ee_template.json");
    const descriptor = await readRegularJsonDocument(file, `EE template ${entry.name}`);
    if (stringValue(descriptor.document?.id) !== entry.name) {
      throw new Error(`EE template '${entry.name}' has drifted identity`);
    }
    const next = structuredClone(descriptor.document);
    filterDescriptorStringList(next, "allowed_interfaces", exactInterfaces, `EE template ${entry.name}`);
    filterDescriptorStringList(next, "allowed_tools", ownedTools, `EE template ${entry.name}`);
    filterDescriptorStringList(next, "hard_blocked_tools", ownedTools, `EE template ${entry.name}`);
    if (next.interface_policy_overrides !== undefined) {
      if (!isPlainRecord(next.interface_policy_overrides)) {
        throw new Error(`EE template '${entry.name}'.interface_policy_overrides must be an object`);
      }
      for (const interfaceId of exactInterfaces) delete next.interface_policy_overrides[interfaceId];
    }
    if (stableJson(next) !== stableJson(descriptor.document)) {
      writes.push({ file, beforeDigest: descriptor.digest, next });
    }
  }

  return writes;
}

function filterDescriptorStringList(document, field, removed, label) {
  if (document[field] === undefined || document[field] === null) return;
  if (!Array.isArray(document[field]) || document[field].some((value) => !stringValue(value))) {
    throw new Error(`${label}.${field} must be an array of non-empty strings`);
  }
  document[field] = document[field].filter((value) => !removed.has(value));
}

async function assertNoExternalTaskTopologyReferences(
  registryRoot,
  excludedDirectories,
  plannedDocuments,
  topologyIds,
  lifecycle = null,
) {
  const files = await registryControlJsonFiles(registryRoot, excludedDirectories);
  for (const file of files) {
    if (plannedDocuments.has(file) && plannedDocuments.get(file) === null) continue;
    const planned = plannedDocuments.get(file);
    let document = planned ?? (await readRegularJsonDocument(file, `registry control document ${file}`)).document;
    if (lifecycle && path.basename(file) === "artifact.json"
      && path.dirname(path.dirname(file)) === path.join(registryRoot, "artifact")
      && document.collection_catalogue !== undefined) {
      const canonicalContentHash = (value) => Buffer.from(blake3(Buffer.from(stableJson(value)))).toString("hex");
      document = lifecycle.catalogueProvenanceProjection(document, {
        scope: document.collection_catalogue?.scope,
        snapshot: { id: document.id, content: canonicalContentHash(document) },
      }, canonicalContentHash);
    }
    const references = exactStringReferencePaths(document, topologyIds);
    if (references.length) {
      throw new Error(`task topology is still referenced by '${file}' at ${references.slice(0, 4).join(", ")}`);
    }
  }
}

async function registryControlJsonFiles(registryRoot, excludedDirectories) {
  const files = [];
  const rootEntries = await fs.readdir(registryRoot, { withFileTypes: true });
  for (const rootEntry of rootEntries) {
    const rootPath = path.join(registryRoot, rootEntry.name);
    if (rootEntry.isFile() && rootEntry.name.endsWith(".json")) {
      files.push(rootPath);
      continue;
    }
    if (!rootEntry.isDirectory()) continue;
    for (const entry of await fs.readdir(rootPath, { withFileTypes: true })) {
      if (!entry.isDirectory()) continue;
      const directory = path.join(rootPath, entry.name);
      if (excludedDirectories.has(directory)) continue;
      for (const file of await fs.readdir(directory, { withFileTypes: true })) {
        if (file.isFile() && file.name.endsWith(".json")) files.push(path.join(directory, file.name));
      }
    }
  }
  return files.sort();
}

function exactStringReferencePaths(value, targets, cursor = "$", found = []) {
  if (typeof value === "string") {
    if (targets.has(value)) found.push(cursor);
    return found;
  }
  if (Array.isArray(value)) {
    value.forEach((entry, index) => exactStringReferencePaths(entry, targets, `${cursor}[${index}]`, found));
    return found;
  }
  if (!isPlainRecord(value)) return found;
  for (const [key, entry] of Object.entries(value)) {
    if (targets.has(key)) found.push(`${cursor}{${JSON.stringify(key)}}`);
    exactStringReferencePaths(entry, targets, `${cursor}.${key}`, found);
  }
  return found;
}

async function snapshotOwnedInterfaceStateDirectories(octopusHomeRoot, exactInterfaces) {
  return [
    ...await snapshotOwnedInterfaceDirectories(
      octopusHomeRoot,
      "interfaces",
      exactInterfaces,
      "interface private state",
    ),
    ...await snapshotOwnedInterfaceDirectories(
      octopusHomeRoot,
      "interface-hand-in",
      exactInterfaces,
      "interface hand-in state",
    ),
  ];
}

async function assertTaskWorkerEnvironmentsReleased(octopusHomeRoot, exactInterfaces, runId, ownedWriters = new Map(), sourceRecord = null) {
  for (const interfaceId of [...exactInterfaces].sort()) {
    const root = path.join(
      octopusHomeRoot,
      "interfaces",
      interfaceId,
      "worker-environments",
    );
    const entries = await readDirectoryEntries(root);
    for (const entry of entries) {
      if (!entry.isDirectory() || !/^we_[0-9a-f]{32}$/.test(entry.name)) {
        throw new Error(`task interface '${interfaceId}' has malformed worker-environment state`);
      }
      const recordPath = path.join(root, entry.name, "worker_environment.json");
      const record = await readOptionalRegularJsonDocument(
        recordPath,
        `worker environment ${entry.name}`,
      );
      if (!record) throw new Error(`worker environment '${entry.name}' has no lifecycle record`);
      const formation = objectOrEmpty(record.document?.formation);
      const writer = ownedWriters.get(formation.run);
      const writerTarget = writer?.document?.interface_state?.[writer.owner]?.file_writer_target;
      const ownedWriter = writer && writerTarget?.run === (writer.targetRun ?? runId) && writerTarget.door === interfaceId
        && formation.worker === writer.document.worker;
      const ownedCompanion = writer && ["observer", "projection"].includes(writer.role)
        && writer.interfaces.has(interfaceId)
        && formation.worker === writer.document.worker;
      const ownedSource = stringValue(formation.run) === runId
        && (!sourceRecord || formation.worker === sourceRecord.worker);
      if (
        record.document?.schema_version !== 5
        || stringValue(formation.worker_environment_id) !== entry.name
        || stringValue(formation.door_interface) !== interfaceId
        || (!ownedSource && !ownedWriter && !ownedCompanion)
        || typeof formation.requires_release !== "boolean"
        || !["active", "failed", "released"].includes(stringValue(record.document?.lifecycle))
      ) {
        throw new Error(`worker environment '${entry.name}' does not belong to task '${runId}'`);
      }
      if (
        formation.requires_release === true
        && stringValue(record.document.lifecycle) !== "released"
      ) {
        throw new Error(
          `task '${runId}' cannot be deleted before worker environment '${entry.name}' is released`,
        );
      }
    }
  }
}

async function snapshotReleasedRunWorkerEnvironmentStateDirectories(
  octopusHomeRoot,
  run,
  runId,
) {
  const runWorker = requiredSafeRegistryId(run?.worker, `read-only task '${runId}' worker`);
  const interfacesRoot = path.join(octopusHomeRoot, "interfaces");
  const records = await listWorkerEnvironmentRecordFiles(interfacesRoot);
  const directories = [];
  const ids = [];
  for (const recordPath of records) {
    const record = await readRegularJsonDocument(recordPath, `worker environment ${recordPath}`);
    const formation = objectOrEmpty(record.document?.formation);
    if (stringValue(formation.run) !== runId) continue;
    const relative = path.relative(interfacesRoot, recordPath).split(path.sep);
    const interfaceId = requiredSafeRegistryId(relative[0], "worker environment door interface");
    const workerEnvironmentId = requiredSafeRegistryId(
      formation.worker_environment_id,
      "worker environment id",
    );
    const lifecycle = stringValue(record.document?.lifecycle);
    if (
      relative.length !== 4
      || relative[1] !== "worker-environments"
      || relative[2] !== workerEnvironmentId
      || relative[3] !== "worker_environment.json"
      || !/^we_[0-9a-f]{32}$/.test(workerEnvironmentId)
      || record.document?.schema_version !== 5
      || !validWorkerEnvironmentFormation(formation)
      || stringValue(formation.door_interface) !== interfaceId
      || stringValue(formation.worker) !== runWorker
      || !["active", "failed", "released"].includes(lifecycle)
    ) {
      throw new Error(
        `worker environment '${workerEnvironmentId}' does not belong to read-only task '${runId}'`,
      );
    }
    if (lifecycle !== "released") {
      throw new Error(
        `read-only task '${runId}' cannot be deleted while worker environment '${workerEnvironmentId}' is '${lifecycle}'`,
      );
    }
    directories.push(await snapshotRemovalDirectory(
      path.dirname(recordPath),
      `read-only task worker environment ${workerEnvironmentId}`,
      true,
    ));
    ids.push(workerEnvironmentId);
  }
  return { directories, ids: ids.sort() };
}

async function snapshotOwnedInterfaceDirectories(
  octopusHomeRoot,
  rootName,
  exactInterfaces,
  label,
) {
  const root = path.join(octopusHomeRoot, rootName);
  let rootStat;
  try {
    rootStat = await fs.lstat(root);
  } catch (error) {
    if (error?.code === "ENOENT") return [];
    throw error;
  }
  if (rootStat.isSymbolicLink() || !rootStat.isDirectory()) {
    throw new Error(`Octopus ${label} root must be a directory and not a symbolic link`);
  }
  const directories = [];
  for (const interfaceId of [...exactInterfaces].sort()) {
    const directory = path.join(root, interfaceId);
    let stat;
    try {
      stat = await fs.lstat(directory);
    } catch (error) {
      if (error?.code === "ENOENT") continue;
      throw error;
    }
    if (stat.isSymbolicLink() || !stat.isDirectory()) {
      throw new Error(`${label} '${interfaceId}' must be a directory and not a symbolic link`);
    }
    directories.push(removalSnapshot(directory, "directory", stat, `${label} ${interfaceId}`));
  }
  return directories;
}

async function snapshotRemovalDirectory(directory, label, captureDirectJson) {
  const stat = await fs.lstat(directory);
  if (stat.isSymbolicLink() || !stat.isDirectory()) {
    throw new Error(`${label} must be a directory and not a symbolic link`);
  }
  const snapshot = removalSnapshot(directory, "directory", stat, label);
  if (captureDirectJson) snapshot.directJsonDigests = await directJsonDigests(directory, label);
  return snapshot;
}

async function snapshotRemovalFile(file, expectedDigest, label) {
  const stat = await fs.lstat(file);
  if (stat.isSymbolicLink() || !stat.isFile()) throw new Error(`${label} must be a regular file`);
  const document = await readRegularJsonDocument(file, label);
  if (document.digest !== expectedDigest) throw new Error(`${label} changed before deletion`);
  return { ...removalSnapshot(file, "file", stat, label), expectedDigest };
}

function removalSnapshot(targetPath, type, stat, label) {
  return {
    path: targetPath,
    type,
    dev: stat.dev,
    ino: stat.ino,
    label,
  };
}

async function directJsonDigests(directory, label) {
  const digests = new Map();
  for (const entry of await fs.readdir(directory, { withFileTypes: true })) {
    if (!entry.isFile() || !entry.name.endsWith(".json")) continue;
    const file = path.join(directory, entry.name);
    const document = await readRegularJsonDocument(file, `${label} control ${entry.name}`);
    digests.set(entry.name, document.digest);
  }
  return digests;
}

async function requireUnchangedRemoval(removal) {
  const stat = await fs.lstat(removal.path);
  const typeMatches = removal.type === "directory" ? stat.isDirectory() : stat.isFile();
  if (stat.isSymbolicLink() || !typeMatches || stat.dev !== removal.dev || stat.ino !== removal.ino) {
    throw new Error(`${removal.label} changed before deletion`);
  }
  if (removal.expectedDigest) {
    await requireUnchangedJsonDigest(removal.path, removal.expectedDigest, removal.label);
  }
  if (removal.directJsonDigests) {
    const current = await directJsonDigests(removal.path, removal.label);
    if (
      current.size !== removal.directJsonDigests.size
      || [...removal.directJsonDigests].some(([name, digest]) => current.get(name) !== digest)
    ) {
      throw new Error(`${removal.label} control documents changed before deletion`);
    }
  }
  if (removal.directFileDigests) {
    const current = new Map();
    for (const entry of await fs.readdir(removal.path, { withFileTypes: true })) {
      if (entry.name === ".octopus-publication") { await requirePrivatePublicationDirectory(path.join(removal.path, entry.name)); continue; }
      if (!entry.isFile() || entry.isSymbolicLink()) {
        throw new Error(`${removal.label} contains a non-file entry before deletion`);
      }
      const bytes = await fs.readFile(path.join(removal.path, entry.name));
      current.set(entry.name, crypto.createHash("sha256").update(bytes).digest("hex"));
    }
    if (
      current.size !== removal.directFileDigests.size
      || [...removal.directFileDigests].some(([name, digest]) => current.get(name) !== digest)
    ) {
      throw new Error(`${removal.label} files changed before deletion`);
    }
  }
}

async function requirePrivatePublicationDirectory(directory) {
  const stat = await fs.lstat(directory);
  if (!stat.isDirectory() || stat.isSymbolicLink() || (stat.mode & 0o077) !== 0 || await fs.realpath(directory) !== directory) {
    throw new Error("Publication metadata must be a plain private directory");
  }
}

async function assertNoRunRecordReferencesEe(octopusHomeRoot, eeId) {
  const runRoot = await fs.realpath(path.join(octopusHomeRoot, "state", "runs"));
  for (const file of await listDirectFiles(runRoot, isRunRecordJsonPath)) {
    const candidate = await readRegularJsonDocument(file, `run record ${file}`);
    if (stringValue(candidate.document?.ee) === eeId) {
      throw new Error(`EE '${eeId}' is still referenced by run '${stringValue(candidate.document?.id)}'`);
    }
  }
}
