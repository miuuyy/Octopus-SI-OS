module.exports = { assertGenericOperatorChatArtifactAllowed, planOperatorChatSurface, planOperatorChatTaskAmendment, submitOperatorTurnControl };

const fs = require("node:fs/promises");
const path = require("node:path");
const { assertExactOperatorClientInterface } = require("./artifactBatches.cjs");
const { chatIdentityFromRunRecord, readConversationState, resolveContinuationState } = require("./conversation.cjs");
const { resolveRunRecordsRoot, writeContentAddressedPayload } = require("./operatorMessages.cjs");
const { runOperatorRevisionCommand } = require("./operatorTurns.cjs");
const { validatePlannedHostBindings } = require("./hostBindings.cjs");
const { readFileHashIfPresent, resolveRegistryRoot, waitForInterfaceSocket, withRunAuthoringLock } = require("./publication.cjs");
const { readConfig, resolveEeInstancesRoot, resolveInstallationRoot, resolvePackageExecutable } = require("./roots.cjs");
const { assertPublicationExpansion, interfaceEndpoint, operatorContextArtifactBindings, operatorDoorHostSlots, operatorExternalInputs, operatorExternalRelays, operatorModelOutputs, operatorPayloadArtifactBindings, operatorPublications, operatorSelectedTools, operatorSelectedWorkerEnvironmentKinds, operatorSurfaceRole, planOperatorChatTopology, plannedAdditionalRunWrites, plannedClientRemoval, plannedClientWrite, plannedExactIds, safeContractToken, safeSha256, validatePlannedRunRecord } = require("./topologyPlan.cjs");
const { absolutePath, assertAdmitted, isObject, numberValue, optionalServiceTier, randomToken, readJsonDocumentIfPresent, requiredArray, requiredFunction, requiredObject, requiredString, safeSegment, sha256Hex, stringValue } = require("./values.cjs");

async function submitOperatorTurnControl(options, request) {
  const config = await readConfig();
  const octopusHome = absolutePath(options.octopusHome(), "octopusHome");
  const interfaceId = safeSegment(request?.interfaceId, "interfaceId");
  const runId = safeSegment(request?.runId, "runId");
  const controlId = safeContractToken(request?.controlId, "controlId");
  const action = requiredString(request?.action, "action").toLowerCase();
  if (!["interrupt", "resume", "stop"].includes(action)) {
    throw new Error("operator turn control action must be 'interrupt', 'resume', or 'stop'");
  }
  const timeoutMs = numberValue(request?.timeoutMs, 45_000);
  const runsDir = await resolveRunRecordsRoot(octopusHome);
  const runDocument = await readJsonDocumentIfPresent(path.join(runsDir, `${runId}.json`));
  if (!runDocument) throw new Error(`operator chat run '${runId}' does not exist`);
  const identity = chatIdentityFromRunRecord(config, runId, runDocument.value);
  if (identity.interfaceId !== interfaceId) {
    throw new Error(`operator chat run '${runId}' is not owned by interface '${interfaceId}'`);
  }
  await assertExactOperatorClientInterface(octopusHome, interfaceId);
  await waitForInterfaceSocket(octopusHome, interfaceId, timeoutMs);
  const artifactRoot = path.join(
    absolutePath(options.clientArtifactsRoot(interfaceId), "clientArtifactsRoot"),
    "operator-chat",
    "payloads",
  );
  await fs.mkdir(artifactRoot, { recursive: true });
  const label = requiredString(config.artifact_labels.turn_control, "client.artifact_labels.turn_control");
  const payload = { action, control_id: controlId };
  const payloadPath = await writeContentAddressedPayload(
    artifactRoot,
    label,
    Buffer.from(JSON.stringify(payload)),
  );
  const verdict = await requiredFunction(options.handInArtifact, "handInArtifact")({
    endpoint: interfaceEndpoint(octopusHome, interfaceId),
    interfaceId,
    manifest: { label, payload_path: payloadPath },
    timeoutMs,
  });
  assertAdmitted(verdict, label);
  return {
    interfaceId,
    runId,
    label,
    controlId,
    action,
    payloadPath,
    verdict,
  };
}

async function assertGenericOperatorChatArtifactAllowed(request) {
  const config = await readConfig();
  const interfaceId = safeSegment(request?.interfaceId, "interfaceId");
  const label = requiredString(request?.label, "label");
  const interfacePrefix = requiredString(config.run.interface_prefix, "client.run.interface_prefix");
  if (!interfaceId.startsWith(interfacePrefix)) return;
  const reservedLabels = new Set([
    requiredString(config.artifact_labels.message, "client.artifact_labels.message"),
    requiredString(config.artifact_labels.turn_control, "client.artifact_labels.turn_control"),
  ]);
  if (reservedLabels.has(label)) {
    throw new Error(`operator chat artifact '${label}' requires its interface-owned admission path`);
  }
}

async function planOperatorChatSurface(options, request) {
  const allowedFields = new Set([
    "chatId",
    "cycle",
    "providerInterfaceId",
    "providerModel",
    "reasoningEffort",
    "serviceTier",
    "voiceModel",
    "eeId",
    "workerInterfaceId",
    "doorHostSlots",
    "externalInputs",
    "externalRelays",
    "modelOutputs",
    "contextArtifactBindings",
    "payloadArtifactBindings",
    "publications",
    "selectedWorkerEnvironmentKinds",
    "selectedTools",
    "clientSurfaceRole",
    "continuation",
  ]);
  if (!isObject(request)) throw new Error("operator chat surface plan must be a JSON object");
  const unknownFields = Object.keys(request).filter((field) => !allowedFields.has(field));
  if (unknownFields.length > 0) {
    throw new Error(`operator chat surface plan contains unknown fields: ${unknownFields.join(", ")}`);
  }
  const config = await readConfig();
  const octopusHome = absolutePath(options.octopusHome(), "octopusHome");
  const registryRoot = resolveRegistryRoot(options);
  const runsDir = await resolveRunRecordsRoot(octopusHome);
  const eeInstancesRoot = await resolveEeInstancesRoot(octopusHome, options);
  const chatId = safeSegment(request.chatId, "chatId");
  const cycle = safeSegment(request.cycle, "cycle");
  const providerInterfaceId = safeSegment(
    request.providerInterfaceId,
    "providerInterfaceId",
  );
  const providerModel = requiredString(request.providerModel, "providerModel");
  const clientSurfaceRole = operatorSurfaceRole(request.clientSurfaceRole);
  const eeId = request.eeId === undefined || request.eeId === null
    ? safeSegment(config.run.ee, "client.run.ee")
    : safeSegment(request.eeId, "eeId");
  const workerInterfaceId = request.workerInterfaceId === undefined
      || request.workerInterfaceId === null
    ? safeSegment(config.run.worker, "client.run.worker")
    : safeSegment(request.workerInterfaceId, "workerInterfaceId");
  const runId = `${requiredString(config.run.run_prefix, "client.run.run_prefix")}${chatId}`;
  const chatInterfaceId = `${requiredString(
    config.run.interface_prefix,
    "client.run.interface_prefix",
  )}${chatId}`;
  const chatRunPath = path.join(runsDir, `${runId}.json`);
  if (await readJsonDocumentIfPresent(chatRunPath)) {
    throw new Error(`operator chat surface plan refuses existing run '${runId}'`);
  }
  const continuation = await resolveContinuationState({
    config,
    octopusHome,
    requestedRunId: "",
    request: request.continuation,
  });
  const conversationState = await readConversationState({
    octopusHome,
    existing: null,
    runId,
    interfaceId: chatInterfaceId,
    terminalArtifactLabel: config.artifact_labels.terminal_output,
    errorArtifactLabel: config.artifact_labels.error_output,
    continuation,
  });
  const doorHostSlots = operatorDoorHostSlots(request.doorHostSlots);
  const externalInputs = operatorExternalInputs(request.externalInputs);
  const externalRelays = operatorExternalRelays(request.externalRelays);
  const modelOutputs = operatorModelOutputs(request.modelOutputs);
  const contextArtifactBindings = operatorContextArtifactBindings(
    request.contextArtifactBindings,
  );
  const payloadArtifactBindings = operatorPayloadArtifactBindings(request.payloadArtifactBindings);
  const publications = operatorPublications(request.publications);
  const selectedWorkerEnvironmentKinds = operatorSelectedWorkerEnvironmentKinds(
    request.selectedWorkerEnvironmentKinds,
  );
  const selectedTools = operatorSelectedTools(request.selectedTools);
  const topologyPlan = await planOperatorChatTopology({
    octopusHome,
    topologyPlanner: resolvePackageExecutable(
      config,
      resolveInstallationRoot(options),
      registryRoot,
      "topology_planner",
    ),
    registryRoot,
    eeInstancesRoot,
    chatId,
    providerInterfaceId,
    providerModel,
    reasoningEffort: stringValue(request.reasoningEffort) || null,
    serviceTier: optionalServiceTier(request.serviceTier),
    voiceModel: request.voiceModel === undefined || request.voiceModel === null ? null : requiredString(request.voiceModel, "voiceModel"),
    eeId,
    workerInterfaceId,
    doorHostSlots,
    externalInputs,
    externalRelays,
    modelOutputs,
    contextArtifactBindings,
    payloadArtifactBindings,
    publications,
    selectedWorkerEnvironmentKinds,
    selectedTools,
    clientSurfaceRole,
    cycle,
    existingRunRecord: null,
    conversation: {
      ledger_offset: conversationState.ledgerOffset,
      artifact_bindings: conversationState.artifactBindings,
      pinned_artifact_bindings: conversationState.pinnedArtifactBindings,
      pinned_context_items: conversationState.pinnedContextItems,
      continued_from: conversationState.continuedFrom,
    },
  });
  if (topologyPlan.expected_before_hash_algorithm !== "sha256") {
    throw new Error("topology plan did not honor the requested SHA-256 write contract");
  }
  if (topologyPlan.run_id !== runId || topologyPlan.chat_interface_id !== chatInterfaceId) {
    throw new Error(
      `topology plan identity drifted: planned run '${topologyPlan.run_id}' interface '${topologyPlan.chat_interface_id}', expected run '${runId}' interface '${chatInterfaceId}'`,
    );
  }
  const expectedAuthoringProfile = {
    ee_id: eeId,
    worker_choice_interface: workerInterfaceId,
    door_host_slots: doorHostSlots,
    ...(externalInputs.length > 0 ? { external_inputs: externalInputs } : {}),
    ...(externalRelays.length > 0 ? { external_relays: externalRelays } : {}),
    ...(modelOutputs.length > 0 ? { model_outputs: modelOutputs } : {}),
    ...(contextArtifactBindings.length > 0
      ? { context_artifact_bindings: contextArtifactBindings }
      : {}),
    ...(payloadArtifactBindings.length > 0 ? { payload_artifact_bindings: payloadArtifactBindings } : {}),
    ...(selectedWorkerEnvironmentKinds.length > 0
      ? { selected_worker_environment_kinds: selectedWorkerEnvironmentKinds }
      : {}),
    ...(selectedTools.length > 0 ? { selected_tools: selectedTools } : {}),
  };
  const authoringProfile = requiredObject(
    topologyPlan.authoring_profile,
    "plan.authoring_profile",
  );
  assertPublicationExpansion(topologyPlan, expectedAuthoringProfile, publications);
  const requiredInterfaceIds = plannedExactIds(
    topologyPlan.required_interface_ids,
    "plan.required_interface_ids",
    safeSegment,
  );
  const requiredToolIds = plannedExactIds(
    topologyPlan.required_tool_ids,
    "plan.required_tool_ids",
    safeContractToken,
  );
  for (const required of [
    topologyPlan.chat_interface_id,
    topologyPlan.model_interface_id,
    topologyPlan.worker_interface_id,
  ]) {
    if (!requiredInterfaceIds.includes(required)) {
      throw new Error(`topology plan required interfaces omit '${required}'`);
    }
  }
  const writes = topologyPlan.interface_writes.map((write, index) => (
    plannedClientWrite(write, index, registryRoot)
  ));
  writes.push(...plannedAdditionalRunWrites(topologyPlan, runsDir, registryRoot));
  const runRecordContent = topologyPlan.run_record_content;
  if (typeof runRecordContent !== "string" || !runRecordContent.trim()) {
    throw new Error("plan.run_record_content must be a non-empty string");
  }
  validatePlannedRunRecord(
    runRecordContent,
    runId,
    chatInterfaceId,
    eeId,
    topologyPlan.worker_interface_id,
    clientSurfaceRole,
  );
  writes.push({
    filePath: chatRunPath,
    content: runRecordContent,
    tag: `run:${runId}`,
    expectedBeforeHash: null,
  });
  const writePaths = new Set();
  for (const write of writes) {
    if (writePaths.has(write.filePath)) {
      throw new Error(`topology plan repeats write path '${write.filePath}'`);
    }
    writePaths.add(write.filePath);
  }
  return {
    runId,
    chatInterfaceId,
    modelInterfaceId: safeSegment(topologyPlan.model_interface_id, "plan.model_interface_id"),
    providerModel: requiredString(topologyPlan.model, "plan.model"),
    workerInterfaceId: safeSegment(topologyPlan.worker_interface_id, "plan.worker_interface_id"),
    clientSurfaceRole: operatorSurfaceRole(topologyPlan.client_surface_role),
    reasoningEffort: stringValue(topologyPlan.reasoning_effort),
    serviceTier: optionalServiceTier(topologyPlan.service_tier),
    authoringProfile,
    requiredInterfaceIds,
    requiredToolIds,
    writes,
    ledgerOffset: conversationState.ledgerOffset,
  };
}

async function planOperatorChatTaskAmendment(options, request) {
  const config = await readConfig();
  const octopusHome = absolutePath(options.octopusHome(), "octopusHome");
  const registryRoot = resolveRegistryRoot(options);
  const runsDir = await resolveRunRecordsRoot(octopusHome);
  const eeInstancesRoot = await resolveEeInstancesRoot(octopusHome, options);
  const runId = safeSegment(request?.runId, "runId");
  const expectedRunHash = safeSha256(request?.expectedRunHash, "expectedRunHash");
  const runPath = path.join(runsDir, `${runId}.json`);
  const current = await readJsonDocumentIfPresent(runPath);
  if (!current || sha256Hex(Buffer.from(current.text)) !== expectedRunHash) {
    throw new Error(`operator chat run '${runId}' changed before capability amendment`);
  }
  const identity = chatIdentityFromRunRecord(config, runId, current.value);
  const clientState = requiredObject(
    requiredObject(current.value.interface_state, "run.interface_state")[identity.interfaceId],
    `run.interface_state.${identity.interfaceId}`,
  );
  const providerChoiceInterfaceId = safeSegment(
    clientState.provider_choice_interface,
    "client provider choice",
  );
  const providerInterfaceId = safeSegment(
    clientState.provider_runtime_interface,
    "client provider runtime",
  );
  const workerInterfaceId = safeSegment(
    clientState.worker_choice_interface,
    "client worker choice",
  );
  const clientSurfaceRole = operatorSurfaceRole(clientState.client_surface_role);
  const eeId = safeSegment(current.value.ee, "run.ee");
  const selectedTools = operatorSelectedTools(request?.selectedTools);
  const externalInputs = operatorExternalInputs(request?.externalInputs);
  const externalRelays = operatorExternalRelays(request?.externalRelays);
  const modelOutputs = operatorModelOutputs(request?.modelOutputs);
  const contextArtifactBindings = operatorContextArtifactBindings(request?.contextArtifactBindings);
  const publications = operatorPublications(request?.publications);
  const selectedWorkerEnvironmentKinds = operatorSelectedWorkerEnvironmentKinds(
    request?.selectedWorkerEnvironmentKinds,
  );
  const doorHostSlots = operatorDoorHostSlots(request?.doorHostSlots);
  const providerDescriptorPath = path.join(
    registryRoot,
    "interface",
    providerInterfaceId,
    "interface.json",
  );
  const providerDescriptor = await readJsonDocumentIfPresent(providerDescriptorPath);
  if (!providerDescriptor) {
    throw new Error(`operator chat provider '${providerInterfaceId}' has no exact descriptor`);
  }
  const providerPrivateConfig = requiredObject(
    requiredObject(
      providerDescriptor.value.interface_driver_config,
      "provider interface_driver_config",
    ).private_config,
    "provider private_config",
  );
  const providerModel = requiredString(providerPrivateConfig.model, "provider model");
  const reasoningEffort = stringValue(providerPrivateConfig.reasoning_effort);
  const serviceTier = optionalServiceTier(providerPrivateConfig.service_tier);
  const conversationState = await readConversationState({
    octopusHome,
    existing: current.value,
    runId,
    interfaceId: identity.interfaceId,
    terminalArtifactLabel: config.artifact_labels.terminal_output,
    errorArtifactLabel: config.artifact_labels.error_output,
    operatorMessageArtifactLabel: config.artifact_labels.message,
    continuation: null,
  });
  const topologyPlan = await planOperatorChatTopology({
    octopusHome,
    topologyPlanner: resolvePackageExecutable(
      config,
      resolveInstallationRoot(options),
      registryRoot,
      "topology_planner",
    ),
    registryRoot,
    eeInstancesRoot,
    chatId: identity.chatId,
    providerInterfaceId: providerChoiceInterfaceId,
    providerModel,
    reasoningEffort,
    serviceTier,
    eeId,
    workerInterfaceId,
    doorHostSlots,
    externalInputs,
    externalRelays,
    modelOutputs,
    contextArtifactBindings,
    publications,
    selectedWorkerEnvironmentKinds,
    selectedTools,
    clientSurfaceRole,
    cycle: randomToken(24),
    existingRunRecord: current.value,
    conversation: {
      ledger_offset: conversationState.ledgerOffset,
      artifact_bindings: conversationState.artifactBindings,
      pinned_artifact_bindings: conversationState.pinnedArtifactBindings,
      pinned_context_items: conversationState.pinnedContextItems,
      continued_from: conversationState.continuedFrom,
    },
  });
  if (
    topologyPlan.expected_before_hash_algorithm !== "sha256"
    || topologyPlan.run_id !== runId
    || topologyPlan.chat_interface_id !== identity.interfaceId
    || topologyPlan.model_interface_id !== providerInterfaceId
    || topologyPlan.model !== providerModel
    || topologyPlan.reasoning_effort !== reasoningEffort
    || (serviceTier && topologyPlan.service_tier !== serviceTier)
  ) {
    throw new Error("operator chat capability amendment changed its exact task identity");
  }
  const writes = topologyPlan.interface_writes.map((write, index) => (
    plannedClientWrite(write, index, registryRoot)
  ));
  writes.push(...plannedAdditionalRunWrites(topologyPlan, runsDir, registryRoot));
  const plannedProviderWrite = writes.find((write) => write.filePath === providerDescriptorPath);
  const providerDescriptorHash = sha256Hex(Buffer.from(providerDescriptor.text));
  if (plannedProviderWrite && plannedProviderWrite.expectedBeforeHash !== providerDescriptorHash) {
    throw new Error("operator chat capability amendment did not preserve its exact provider descriptor revision");
  }
  validatePlannedRunRecord(
    topologyPlan.run_record_content,
    runId,
    identity.interfaceId,
    eeId,
    topologyPlan.worker_interface_id,
    clientSurfaceRole,
  );
  writes.push({
    filePath: runPath,
    content: topologyPlan.run_record_content,
    tag: `run:${runId}`,
    expectedBeforeHash: expectedRunHash,
  });
  await validatePlannedHostBindings({ octopusHome, registryRoot, plan: { writes, requiredToolIds: topologyPlan.required_tool_ids } });
  return {
    runId,
    writes,
    removals: requiredArray(topologyPlan.removals, "plan.removals").map((removal, index) => (
      plannedClientRemoval(removal, index, registryRoot)
    )),
  };
}
