module.exports = { applicationTaskCheckpointId, configureOctopusOsTaskApplication, plannedTaskApplicationRemovals, plannedTaskApplicationWrites, readInstalledApplicationManifestDocuments };

const fs = require("node:fs/promises");
const crypto = require("node:crypto");
const path = require("node:path");
const { installedDescriptorIds } = require("./applicationConfiguration.cjs");
const { applicationHasAnyAuthoredBinding, applicationIsConnectedToAuthoredRun, taskBaseAuthoringProfile, taskProviderContextArtifactIds } = require("./applicationPackages.cjs");
const { projectOctopusOsRequiredVerifiedArtifacts } = require("./authoringProfiles.cjs");
const { readOptionalRegularJsonDocument, readRegularJsonDocument, requireUnchangedJsonDigest } = require("./jsonFiles.cjs");
const { parseApplicationManifest, parseOctopusOsSidecar } = require("./sidecars.cjs");
const { readClientTaskArchive } = require("./taskArchive.cjs");
const { assertRunPrivateStateIsOwned, assertTaskWorkerPinRemainsTaskLocal, taskOwnedInterfaceIds, requirePrivatePublicationDirectory, scopedRecordPath, snapshotRemovalDirectory } = require("./taskDeletion.cjs");
const { arrayValue, isPlainRecord, mapConcurrentOrdered, numberValue, objectOrEmpty, requiredAbsolutePath, requiredBoolean, requiredDigest, requiredSafeRegistryId, requiredString, stringValue } = require("./values.cjs");

async function configureOctopusOsTaskApplication(
  registryRoot,
  octopusHomeRoot,
  scope,
  request,
  withTaskAuthoringLock,
  planTaskApplicationAmendment,
  commitClientAuthoring,
) {
  if (typeof withTaskAuthoringLock !== "function") {
    throw new Error("operator chat task authoring lock is unavailable");
  }
  if (typeof planTaskApplicationAmendment !== "function") {
    throw new Error("operator chat task capability planner is unavailable");
  }
  if (typeof commitClientAuthoring !== "function") {
    throw new Error("client authoring transaction is unavailable");
  }
  const runId = requiredSafeRegistryId(request?.runId, "runId");
  const runProjection = scope.clientRuns.find((row) => (
    stringValue(row?.run_id) === runId && stringValue(row?.surface_role) === "task"
  ));
  if (!runProjection) throw new Error(`task '${runId}' is not a writable Octopus OS task`);
  const expectedDigest = requiredDigest(request?.expectedDigest, "expectedDigest");
  const applicationId = requiredSafeRegistryId(request?.applicationId, "applicationId");
  const connected = requiredBoolean(request?.connected, "connected");
  const environment = scope.byId.get(requiredSafeRegistryId(runProjection.ee_id, "task EE"));
  if (!environment) throw new Error(`task '${runId}' belongs to an EE outside the Octopus OS scope`);
  const applicationPath = path.join(
    await fs.realpath(path.join(registryRoot, "interface")),
    applicationId,
    "application.json",
  );
  const applicationDocument = await readRegularJsonDocument(
    applicationPath,
    `application manifest ${applicationId}`,
  );
  const manifest = parseApplicationManifest(
    applicationDocument.document,
    applicationId,
    applicationPath,
  );
  const installedInterfaceIds = new Set(await installedDescriptorIds(
    path.join(registryRoot, "interface"),
    "interface.json",
    ["id", "rule_id"],
  ));
  const missingInterfaceIds = manifest.interfaces.filter((interfaceId) => (
    !installedInterfaceIds.has(interfaceId)
  ));
  if (connected && missingInterfaceIds.length) {
    throw new Error(
      `application '${applicationId}' references missing interfaces: ${missingInterfaceIds.join(", ")}`,
    );
  }
  const allowedInterfaces = objectOrEmpty(environment.formed_envelope).allowed_interfaces;
  if (
    connected
    && Array.isArray(allowedInterfaces)
    && manifest.interfaces.some((interfaceId) => !allowedInterfaces.includes(interfaceId))
  ) {
    throw new Error(`application '${applicationId}' is not available in task EE '${environment.ee_id}'`);
  }
  return withTaskAuthoringLock(runId, async () => {
    const runPath = await scopedRecordPath(
      octopusHomeRoot,
      runProjection.record_path,
      path.join("state", "runs"),
      `${runId}.json`,
    );
    const current = await readRegularJsonDocument(runPath, `task run ${runId}`);
    if (current.digest !== expectedDigest || stringValue(runProjection.record_digest) !== expectedDigest) {
      throw new Error(`task '${runId}' changed before its application binding could be edited`);
    }
    const next = current.document;
    if (
      stringValue(next.id) !== runId
      || stringValue(next.ee) !== stringValue(runProjection.ee_id)
      || !Array.isArray(next.checkpoints)
      || !Array.isArray(next.active_checkpoints)
      || !isPlainRecord(next.interface_state)
    ) {
      throw new Error(`task '${runId}' has malformed authored topology`);
    }
    const chatInterface = requiredSafeRegistryId(runProjection.interface_id, "task client interface");
    const chatState = objectOrEmpty(next.interface_state[chatInterface]);
    if (readClientTaskArchive(next, chatInterface)) throw new Error(`task '${runId}' is archived; restore it before changing applications`);
    const providerInterface = requiredSafeRegistryId(
      chatState.provider_runtime_interface,
      "task provider interface",
    );
    const providerState = next.interface_state[providerInterface];
    if (!isPlainRecord(providerState)) {
      throw new Error(`task '${runId}' has no exact provider state`);
    }
    if (providerState.pending_handoff !== undefined) {
      if (!isPlainRecord(providerState.pending_handoff) || providerState.pending_handoff.terminal !== true) {
        throw new Error(`task '${runId}' is working; applications can be changed after the turn settles`);
      }
    } else if (numberValue(providerState.authoring_generation, 0) > 0) {
      throw new Error(`task '${runId}' lost its provider handoff evidence`);
    }
    const activeIds = new Set(next.active_checkpoints.map((value) => (
      requiredSafeRegistryId(value, `task '${runId}' active checkpoint`)
    )));
    if (activeIds.size !== next.active_checkpoints.length) {
      throw new Error(`task '${runId}' repeats an active checkpoint`);
    }
    const checkpointIds = new Set();
    for (const checkpoint of next.checkpoints) {
      if (!isPlainRecord(checkpoint)) throw new Error(`task '${runId}' contains a malformed checkpoint`);
      const checkpointId = requiredSafeRegistryId(checkpoint.id, `task '${runId}' checkpoint`);
      if (checkpointIds.has(checkpointId) || stringValue(checkpoint.run) !== runId) {
        throw new Error(`task '${runId}' contains duplicated or foreign checkpoint '${checkpointId}'`);
      }
      checkpointIds.add(checkpointId);
    }
    if ([...activeIds].some((checkpointId) => !checkpointIds.has(checkpointId))) {
      throw new Error(`task '${runId}' activates a checkpoint absent from its record`);
    }
    const exactInterfaces = await taskOwnedInterfaceIds(registryRoot, next, chatInterface, runId);
    assertRunPrivateStateIsOwned(next, exactInterfaces, runId);
    const workerInterface = requiredSafeRegistryId(next.worker, "task worker interface");
    await assertTaskWorkerPinRemainsTaskLocal(
      octopusHomeRoot,
      workerInterface,
      chatInterface,
      next,
      exactInterfaces,
    );
    const installedApplicationDocuments = await readInstalledApplicationManifestDocuments(registryRoot);
    const chatSidecarPath = path.join(
      await fs.realpath(path.join(registryRoot, "interface")),
      chatInterface,
      "octopus_os.json",
    );
    const chatSidecar = await readRegularJsonDocument(chatSidecarPath, `task client sidecar ${chatInterface}`);
    const parsedSidecar = parseOctopusOsSidecar(chatSidecar.document, chatInterface, chatSidecarPath);
    const profile = objectOrEmpty(parsedSidecar.authoring_profile);
    const currentConnected = applicationIsConnectedToAuthoredRun(next, manifest, profile);
    const currentPresent = applicationHasAnyAuthoredBinding(next, manifest);
    if ((connected && currentConnected) || (!connected && !currentPresent)) {
      return {
        action: "configure_task_application",
        runId,
        capabilityId: applicationId,
        connected,
        changed: false,
      };
    }
    const retainedApplicationDocuments = [];
    for (const candidate of installedApplicationDocuments) {
      if (
        candidate.manifest.id === applicationId
        || !applicationHasAnyAuthoredBinding(next, candidate.manifest)
      ) continue;
      const candidateMissingInterfaces = candidate.manifest.interfaces.filter((interfaceId) => (
        !installedInterfaceIds.has(interfaceId)
      ));
      if (
        candidateMissingInterfaces.length
        || !applicationIsConnectedToAuthoredRun(next, candidate.manifest, profile)
      ) {
        throw new Error(
          `task '${runId}' has an incomplete connection to application '${candidate.manifest.id}'; select that application in this task to repair it before changing another application`,
        );
      }
      retainedApplicationDocuments.push(candidate);
    }
    const retainedManifests = retainedApplicationDocuments.map((candidate) => candidate.manifest);
    const selectedManifests = connected
      ? [...retainedManifests, manifest]
      : retainedManifests;
    const baseAuthoringProfile = connected
      ? null
      : taskBaseAuthoringProfile(scope, runProjection, profile);
    const baseExternalInputs = new Set(arrayValue(baseAuthoringProfile?.external_inputs).flatMap((input) => {
      const interfaceId = stringValue(input?.interface_id);
      const checkpointId = stringValue(input?.checkpoint_id);
      return interfaceId && checkpointId ? [`${checkpointId}\0${interfaceId}`] : [];
    }));
    const targetInterfaceIds = new Set(manifest.interfaces);
    const externalInputs = arrayValue(profile.external_inputs).flatMap((input) => {
      const interfaceId = stringValue(input?.interface_id);
      const checkpointId = stringValue(input?.checkpoint_id);
      return interfaceId && checkpointId && (
        !targetInterfaceIds.has(interfaceId)
        || baseExternalInputs.has(`${checkpointId}\0${interfaceId}`)
      )
        ? [{
            checkpointId,
            interfaceId,
            ...(arrayValue(input?.artifact_names).length
              ? { artifactNames: arrayValue(input.artifact_names).map(stringValue) }
              : {}),
            requiredVerifiedArtifacts: projectOctopusOsRequiredVerifiedArtifacts(
              input?.required_verified_artifacts,
            ),
          }]
        : [];
    });
    const applicationCheckpointIds = new Set(externalInputs.map((input) => input.checkpointId));
    const inputInterfaces = new Set(externalInputs.map((input) => input.interfaceId));
    for (const selected of selectedManifests) {
      selected.interfaces.forEach((interfaceId, index) => {
        if (inputInterfaces.has(interfaceId)) return;
        const checkpointId = applicationTaskCheckpointId(selected.id, "input", index);
        if (applicationCheckpointIds.has(checkpointId)) {
          throw new Error(`applications repeat task checkpoint '${checkpointId}'`);
        }
        applicationCheckpointIds.add(checkpointId);
        inputInterfaces.add(interfaceId);
        externalInputs.push({ checkpointId, interfaceId });
      });
    }
    const allApplicationArtifactIds = new Set(installedApplicationDocuments.flatMap((candidate) => [
      ...candidate.manifest.artifact_bindings.map((binding) => binding.artifact_id),
      ...candidate.manifest.interfaces.map((interfaceId) => `artifact-operation-catalog-${interfaceId}`),
    ]));
    const baseContextArtifactIds = new Set(arrayValue(baseAuthoringProfile?.context_artifact_bindings)
      .map((binding) => stringValue(binding?.id))
      .filter(Boolean));
    const contextArtifactBindings = [...new Set([
      ...[...taskProviderContextArtifactIds(next, activeIds, providerInterface)]
        .filter((artifactId) => (
          !allApplicationArtifactIds.has(artifactId)
          || baseContextArtifactIds.has(artifactId)
        )),
      ...selectedManifests.flatMap((selected) => (
        selected.artifact_bindings.map((binding) => binding.artifact_id)
      )),
    ])].sort().map((id) => ({ id }));
    const baseSelectedTools = new Set(arrayValue(baseAuthoringProfile?.selected_tools).flatMap((capability) => {
      const tool = stringValue(capability?.tool);
      const interfaceId = stringValue(capability?.interface);
      const workerEnvironmentKind = stringValue(capability?.worker_environment_kind);
      return tool && interfaceId && workerEnvironmentKind
        ? [`${tool}\0${interfaceId}\0${workerEnvironmentKind}`]
        : [];
    }));
    const selectedTools = arrayValue(profile.selected_tools).flatMap((capability) => {
      const tool = stringValue(capability?.tool);
      const interfaceId = stringValue(capability?.interface);
      const workerEnvironmentKind = stringValue(capability?.worker_environment_kind);
      const exactKey = `${tool}\0${interfaceId}\0${workerEnvironmentKind}`;
      const targetOwns = manifest.task_capabilities.tools.some((candidate) => (
        candidate.tool === tool
        && candidate.interface === interfaceId
        && candidate.worker_environment_kind === workerEnvironmentKind
      ));
      return tool && interfaceId && workerEnvironmentKind
        && (!targetOwns || connected || baseSelectedTools.has(exactKey))
        ? [{ tool, interface: interfaceId, workerEnvironmentKind }]
        : [];
    });
    const selectedToolOwners = new Map();
    for (const capability of selectedTools) {
      selectedToolOwners.set(
        capability.tool,
        `${capability.interface}\0${capability.workerEnvironmentKind}`,
      );
    }
    for (const selected of selectedManifests) {
      for (const capability of selected.task_capabilities.tools) {
        const key = `${capability.interface}\0${capability.worker_environment_kind}`;
        const owner = selectedToolOwners.get(capability.tool);
        if (owner && owner !== key) {
          throw new Error(`applications bind tool '${capability.tool}' to different exact owners`);
        }
        if (!owner) selectedTools.push({
          tool: capability.tool,
          interface: capability.interface,
          workerEnvironmentKind: capability.worker_environment_kind,
        });
        selectedToolOwners.set(capability.tool, key);
      }
    }
    const amendment = await planTaskApplicationAmendment({
      runId,
      expectedRunHash: current.digest,
      doorHostSlots: objectOrEmpty(profile.door_host_slots),
      externalInputs,
      externalRelays: arrayValue(profile.external_relays).map((relay) => ({
        checkpointId: stringValue(relay?.checkpoint_id),
        interfaceId: stringValue(relay?.interface_id),
        outboundInterfaceIds: arrayValue(relay?.outbound_interface_ids).map(stringValue).filter(Boolean),
        requiredVerifiedArtifacts: projectOctopusOsRequiredVerifiedArtifacts(
          relay?.required_verified_artifacts,
        ),
      })),
      modelOutputs: arrayValue(profile.model_outputs).map((output) => ({
        checkpointId: stringValue(output?.checkpoint_id),
        interfaceId: stringValue(output?.interface_id),
        requiredVerifiedArtifact: {
          mark: stringValue(output?.required_verified_artifact?.mark),
          artifactLabel: stringValue(output?.required_verified_artifact?.artifact_label),
          verifier: stringValue(output?.required_verified_artifact?.verifier),
          params: objectOrEmpty(output?.required_verified_artifact?.params),
        },
      })),
      contextArtifactBindings,
      selectedWorkerEnvironmentKinds: arrayValue(profile.selected_worker_environment_kinds),
      selectedTools,
    });
    await requireUnchangedJsonDigest(
      applicationPath,
      applicationDocument.digest,
      `application manifest ${applicationId}`,
    );
    for (const candidate of retainedApplicationDocuments) {
      await requireUnchangedJsonDigest(
        candidate.file,
        candidate.digest,
        `application manifest ${candidate.manifest.id}`,
      );
    }
    const writes = plannedTaskApplicationWrites(amendment?.writes);
    const removals = await plannedTaskApplicationRemovals(amendment?.removals);
    if (!writes.length && !removals.length) {
      return {
        action: "configure_task_application",
        runId,
        capabilityId: applicationId,
        connected,
        changed: false,
      };
    }
    await commitClientAuthoring({
      label: `configure task application ${applicationId}`,
      writes,
      removals,
      publishBarrierPath: runPath,
    });
    return {
      action: "configure_task_application",
      runId,
      capabilityId: applicationId,
      connected,
      changed: true,
    };
  });
}

async function readInstalledApplicationManifestDocuments(registryRoot) {
  const root = await fs.realpath(path.join(registryRoot, "interface"));
  const entries = await fs.readdir(root, { withFileTypes: true });
  const documents = await mapConcurrentOrdered(
    entries.filter((entry) => entry.isDirectory()),
    32,
    async (entry) => {
      const file = path.join(root, entry.name, "application.json");
      const document = await readOptionalRegularJsonDocument(
        file,
        `application manifest ${entry.name}`,
      );
      if (!document) return null;
      return {
        file,
        digest: document.digest,
        manifest: parseApplicationManifest(document.document, entry.name, file),
      };
    },
  );
  return documents.filter(Boolean);
}

function applicationTaskCheckpointId(applicationId, direction, index) {
  const normalized = requiredSafeRegistryId(applicationId, "application id").replace(/\./g, "_");
  const id = `cp_app_${normalized}_${direction}_${index + 1}`;
  return requiredSafeRegistryId(id, "application task checkpoint");
}

function plannedTaskApplicationWrites(value) {
  if (!Array.isArray(value)) throw new Error("task application planner writes must be an array");
  const paths = new Set();
  return value.map((write, index) => {
    if (!isPlainRecord(write)) throw new Error(`task application planner write ${index} must be an object`);
    const file = requiredAbsolutePath(write.filePath, `task application planner write ${index}`);
    if (paths.has(file)) throw new Error(`task application planner repeats write '${file}'`);
    paths.add(file);
    const beforeDigest = write.expectedBeforeHash === null
      ? null
      : requiredDigest(write.expectedBeforeHash, `task application planner write ${index} digest`);
    if (typeof write.content !== "string" || !write.content) {
      throw new Error(`task application planner write ${index} has no JSON content`);
    }
    let next;
    try {
      next = JSON.parse(write.content);
    } catch (error) {
      throw new Error(`task application planner write ${index} is malformed JSON: ${error.message}`);
    }
    if (!isPlainRecord(next)) throw new Error(`task application planner write ${index} must contain an object`);
    return { file, beforeDigest, content: write.content };
  });
}

async function plannedTaskApplicationRemovals(value) {
  if (!Array.isArray(value)) throw new Error("task application planner removals must be an array");
  const paths = new Set();
  const removals = [];
  for (const [index, removal] of value.entries()) {
    if (!isPlainRecord(removal)) throw new Error(`task application planner removal ${index} must be an object`);
    const directory = requiredAbsolutePath(removal.directory, `task application planner removal ${index}`);
    if (paths.has(directory)) throw new Error(`task application planner repeats removal '${directory}'`);
    paths.add(directory);
    const expectedFiles = new Map(arrayValue(removal.files).map((file, fileIndex) => {
      if (!isPlainRecord(file)) throw new Error(`task application removal file ${fileIndex} must be an object`);
      const name = requiredString(file.relativePath, `task application removal file ${fileIndex}`);
      if (path.basename(name) !== name || name.includes("\0")) {
        throw new Error(`task application removal file ${fileIndex} is not local to its directory`);
      }
      return [name, requiredDigest(file.sha256, `task application removal file ${fileIndex} digest`)];
    }));
    if (!expectedFiles.size) throw new Error(`task application planner removal ${index} has no files`);
    const actual = new Map();
    for (const entry of await fs.readdir(directory, { withFileTypes: true })) {
      if (entry.name === ".octopus-publication") { await requirePrivatePublicationDirectory(path.join(directory, entry.name)); continue; }
      if (!entry.isFile() || entry.isSymbolicLink()) {
        throw new Error(`task application removal '${directory}' contains a non-file entry`);
      }
      const bytes = await fs.readFile(path.join(directory, entry.name));
      actual.set(entry.name, crypto.createHash("sha256").update(bytes).digest("hex"));
    }
    if (
      actual.size !== expectedFiles.size
      || [...expectedFiles].some(([name, digest]) => actual.get(name) !== digest)
    ) {
      throw new Error(`task application removal '${directory}' changed after planning`);
    }
    removals.push(await snapshotRemovalDirectory(
      directory,
      requiredString(removal.tag, `task application planner removal ${index} tag`),
      true,
    ));
    removals[removals.length - 1].directFileDigests = expectedFiles;
  }
  return removals;
}
