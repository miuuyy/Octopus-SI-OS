module.exports = { applyOperatorInputRevision, assertPayloadInputCycleReady, assertPreparedOperatorPayloadSelection, loadOperatorTaskLifecycle, operatorInputRevisionPhases, operatorPayloadRunScope, operatorRevisionDeferred, planExistingProviderSelection, prepareOperatorPayloadArtifactBindings, runOperatorRevisionCommand, selectedProviderDescriptorContent, submitOperatorTurn };

const { spawn } = require("node:child_process");
const fs = require("node:fs/promises");
const path = require("node:path");
const { readMessageRevision } = require("./messageRevision.cjs");
const { activeOperatorMessageCheckpoint, chatIdentityFromRunRecord, existingRunSubmission, existingRunTopology, readConversationState, resolveContinuationState } = require("./conversation.cjs");
const { watchLedgerChange } = require("./liveCursors.cjs");
const { admitOperatorMessage, operatorMessageRequestDigest, prepareOperatorMessageAdmission, readExistingOperatorMessageEnvelope, readOrCreateOperatorMessageIntent, withOperatorMessageAdmissionLock } = require("./messageAdmission.cjs");
const { assertOperatorAttachmentInputLimits, materializeAttachments, operatorMessageChatId, operatorMessagePayload, operatorMessagePayloadPath, operatorTurnClientContext, planOperatorAttachments, resolveRunRecordsRoot, writeExactOperatorMessageFile } = require("./operatorMessages.cjs");
const { commitClientWrites, fileExists, resolveRegistryRoot, waitForInterfaceSocket, withOperatorWorkspaceTopologyLock, withRunAuthoringLock } = require("./publication.cjs");
const { readConfig, resolveEeInstancesRoot, resolveInstallationRoot, resolvePackageExecutable } = require("./roots.cjs");
const { planOperatorChatSurface } = require("./surfacePlanning.cjs");
const { validatePlannedHostBindings } = require("./hostBindings.cjs");
const { assertPublicationExpansion, canonicalJson, interfaceEndpoint, interfaceSocketIdentity, operatorContextArtifactBindings, operatorDoorHostSlots, operatorPayloadArtifactBindings, operatorSelectedWorkerEnvironmentKinds, operatorSurfaceRole, planOperatorChatTopology, plannedAdditionalRunWrites, plannedClientWrite, safeContractToken, validatePlannedRunRecord } = require("./topologyPlan.cjs");
const { absolutePath, arrayValue, assertAdmitted, nonNegativeInteger, numberValue, optionalServiceTier, randomToken, readJsonDocumentIfPresent, requiredArray, requiredFunction, requiredObject, requiredString, requiredTrimmedString, safeSegment, sha256Hex, stringValue } = require("./values.cjs");

async function submitOperatorTurn(options, request) {
  const config = await readConfig();
  const text = stringValue(request?.text);
  const attachmentInputs = arrayValue(request?.attachments);
  assertOperatorAttachmentInputLimits(attachmentInputs);
  const recoverPreparedMessage = request?.recoverPreparedMessage === true;
  if (recoverPreparedMessage && attachmentInputs.length > 0) {
    throw new Error("recoverPreparedMessage cannot carry transient attachments");
  }
  if (!text && attachmentInputs.length === 0 && !recoverPreparedMessage) {
    throw new Error("operator turn must carry text or an attachment");
  }
  const providerReference = safeSegment(request?.providerInterfaceId, "providerInterfaceId");
  const providerModel = requiredString(request?.providerModel, "providerModel");
  const clientSurfaceRole = operatorSurfaceRole(request?.clientSurfaceRole);
  const messageId = safeContractToken(
    stringValue(request?.messageId) || `msg_${randomToken(20)}`,
    "messageId",
  );
  const cycle = randomToken(24);
  const timeoutMs = numberValue(request?.timeoutMs, 120_000);
  const octopusHome = absolutePath(options.octopusHome(), "octopusHome");
  const runPrefix = requiredString(config.run.run_prefix, "client.run.run_prefix");
  const requestedRunId = stringValue(request?.runId);
  const edit = await readMessageRevision(options, request, config.artifact_labels.message).catch(error => {
    error.deliveryState = "not_sent";
    throw error;
  });
  const continuation = requestedRunId
    ? await resolveContinuationState({
      config,
      octopusHome,
      requestedRunId,
      request: request?.continuation,
    })
    : null;
  const runsDir = await resolveRunRecordsRoot(octopusHome);
  let runId;
  let chatId;
  let chatInterfaceId;
  let existingRunDocument = null;
  let existingConversationState = null;
  let stableExistingSubmission = null;
  if (requestedRunId) {
    runId = safeSegment(requestedRunId, "runId");
    existingRunDocument = await readJsonDocumentIfPresent(path.join(runsDir, `${runId}.json`));
    if (!existingRunDocument) throw new Error(`operator chat run '${runId}' does not exist`);
    const identity = chatIdentityFromRunRecord(config, runId, existingRunDocument.value);
    chatId = identity.chatId;
    chatInterfaceId = identity.interfaceId;
  } else {
    chatId = operatorMessageChatId(messageId);
    runId = `${runPrefix}${chatId}`;
    chatInterfaceId = `${config.run.interface_prefix}${chatId}`;
    existingRunDocument = await readJsonDocumentIfPresent(path.join(runsDir, `${runId}.json`));
  }
  if (existingRunDocument) {
    const identity = chatIdentityFromRunRecord(config, runId, existingRunDocument.value);
    if (identity.chatId !== chatId || identity.interfaceId !== chatInterfaceId) {
      throw new Error(`operator chat run '${runId}' changed its exact client interface`);
    }
    stableExistingSubmission = existingRunTopology(
      config,
      runId,
      existingRunDocument.value,
      chatInterfaceId,
      {
        providerChoiceInterfaceId: providerReference,
        clientSurfaceRole,
        eeId: stringValue(request?.eeId),
        workerInterfaceId: stringValue(request?.workerInterfaceId),
      },
    );
    existingConversationState = await readConversationState({
      octopusHome,
      existing: existingRunDocument.value,
      runId,
      interfaceId: chatInterfaceId,
      terminalArtifactLabel: config.artifact_labels.terminal_output,
      errorArtifactLabel: config.artifact_labels.error_output,
      operatorMessageArtifactLabel: config.artifact_labels.message,
      continuation,
    });
  }
  const clientRoot = absolutePath(
    options.clientArtifactsRoot(chatInterfaceId),
    "clientArtifactsRoot",
  );
  const chatRunPath = path.join(runsDir, `${runId}.json`);

  const artifactRoot = path.join(clientRoot, "operator-chat");
  const payloadRoot = path.join(artifactRoot, "payloads");
  const attachmentRoot = path.join(artifactRoot, "attachments");
  await fs.mkdir(payloadRoot, { recursive: true });
  await fs.mkdir(attachmentRoot, { recursive: true });
  let pendingAttachmentPlans = null;
  let envelope = await withOperatorMessageAdmissionLock(
    chatInterfaceId,
    messageId,
    () => readExistingOperatorMessageEnvelope(options, {
      artifactRoot,
      attachmentInputs,
      attachmentRoot,
      chatInterfaceId,
      clientSurfaceRole: stableExistingSubmission?.clientSurfaceRole ?? clientSurfaceRole,
      label: config.artifact_labels.message,
      messageId,
      modelInterfaceId: stableExistingSubmission?.modelInterfaceId ?? providerReference,
      octopusHome,
      recoverPreparedMessage,
      requestedText: text,
      replacesMessage: edit?.revision,
      runDocumentPresent: Boolean(existingRunDocument),
      runId,
      workerInterfaceId: stableExistingSubmission?.workerInterfaceId
        ?? safeSegment(request?.workerInterfaceId || config.run.worker, "workerInterfaceId"),
    }),
  );
  if (!envelope) {
    if (recoverPreparedMessage) {
      throw new Error(`operator message '${messageId}' has no durable envelope; reattach its files before retrying`);
    } else {
      pendingAttachmentPlans = await planOperatorAttachments(attachmentRoot, attachmentInputs);
      const attachments = edit ? edit.payload.attachments : pendingAttachmentPlans.map((plan) => plan.attachment);
      const clientContext = await operatorTurnClientContext(options, clientSurfaceRole, request?.clientContext);
      const messageBytes = Buffer.from(JSON.stringify({
        text,
        message_id: messageId,
        attachments,
        ...(edit ? { replaces_message: edit.revision } : {}),
        ...(clientContext.length ? { client_context: clientContext } : {}),
      }));
      const messagePath = operatorMessagePayloadPath(payloadRoot, messageId);
      await writeExactOperatorMessageFile(messagePath, messageBytes, "payload");
      envelope = {
        messageBytes,
        messagePath,
        payload: operatorMessagePayload(JSON.parse(messageBytes.toString("utf8")), messageId, attachmentRoot),
        recovered: null,
      };
    }
  }

  let preparedPayloadSelection = null;
  let authoredRequest = request;
  await withOperatorMessageAdmissionLock(chatInterfaceId, messageId, async () => {
    const resolved = { clientSurfaceRole, providerReference };
    const sourceRequestDigest = operatorMessageRequestDigest(config, request, resolved);
    if (options.preparePayloadArtifactBindings !== undefined) {
      if (typeof options.preparePayloadArtifactBindings !== "function") {
        throw new Error("preparePayloadArtifactBindings must be a trusted client callback");
      }
      preparedPayloadSelection = await prepareOperatorPayloadArtifactBindings(options, {
        config, request, existingRunDocument, artifactRoot, chatInterfaceId, messageId, runId,
        clientSurfaceRole, sourceRequestDigest,
      });
      await assertPreparedOperatorPayloadSelection(preparedPayloadSelection, await readJsonDocumentIfPresent(chatRunPath));
      authoredRequest = { ...request, payloadArtifactBindings: preparedPayloadSelection.bindings };
    }
    await readOrCreateOperatorMessageIntent({
      artifactRoot, chatInterfaceId, messageBytes: envelope.messageBytes, messageId, runId,
      requestDigest: operatorMessageRequestDigest(config, authoredRequest, resolved),
      ...(preparedPayloadSelection ? {
        sourceRequestDigest, payloadArtifactBindings: preparedPayloadSelection.bindings,
      } : {}),
    });
  });
  if (envelope.recovered) {
    return {
      runId,
      chatInterfaceId,
      modelInterfaceId: stableExistingSubmission.modelInterfaceId,
      workerInterfaceId: stableExistingSubmission.workerInterfaceId,
      clientSurfaceRole: stableExistingSubmission.clientSurfaceRole,
      label: config.artifact_labels.message,
      messageId,
      payloadPath: envelope.messagePath,
      contentHash: sha256Hex(envelope.messageBytes),
      attachmentCount: envelope.payload.attachments.length,
      clientWrites: [],
      messageRecovered: true,
      messageVerdict: envelope.recovered.verdict,
      outboundCursor: {
        offset: envelope.recovered.outboundCursorOffset,
        seq: null,
      },
    };
  }

  if (pendingAttachmentPlans) {
    await materializeAttachments(pendingAttachmentPlans);
  }

  if (existingRunDocument) {
    const recovered = await withOperatorMessageAdmissionLock(
      chatInterfaceId,
      messageId,
      () => prepareOperatorMessageAdmission(options, {
        artifactRoot,
        chatInterfaceId,
        clientSurfaceRole: stableExistingSubmission.clientSurfaceRole,
        label: config.artifact_labels.message,
        messageBytes: envelope.messageBytes,
        messageId,
        messagePath: envelope.messagePath,
        modelInterfaceId: stableExistingSubmission.modelInterfaceId,
        octopusHome,
        outboundCursorOffset: existingConversationState.ledgerOffset,
        runId,
        workerInterfaceId: stableExistingSubmission.workerInterfaceId,
      }),
    );
    if (recovered) {
      return {
        runId,
        chatInterfaceId,
        modelInterfaceId: stableExistingSubmission.modelInterfaceId,
        workerInterfaceId: stableExistingSubmission.workerInterfaceId,
        clientSurfaceRole: stableExistingSubmission.clientSurfaceRole,
        label: config.artifact_labels.message,
        messageId,
        payloadPath: envelope.messagePath,
        contentHash: sha256Hex(envelope.messageBytes),
        attachmentCount: envelope.payload.attachments.length,
        clientWrites: [],
        messageRecovered: true,
        messageVerdict: recovered.verdict,
        outboundCursor: {
          offset: recovered.outboundCursorOffset,
          seq: null,
        },
      };
    }
  }

  const authorSubmission = () => withRunAuthoringLock(runId, async () => {
    if (edit) await readMessageRevision(options, request, config.artifact_labels.message);
    const currentRunDocument = await readJsonDocumentIfPresent(chatRunPath);
    if (preparedPayloadSelection) {
      await assertPreparedOperatorPayloadSelection(preparedPayloadSelection, currentRunDocument);
    }
    if (!currentRunDocument) {
      if (requestedRunId) {
        throw new Error(`operator chat run '${runId}' no longer exists`);
      }
      const plan = await planOperatorChatSurface(options, {
        chatId,
        cycle,
        providerInterfaceId: providerReference,
        providerModel,
        reasoningEffort: request?.reasoningEffort,
        serviceTier: request?.serviceTier,
        eeId: request?.eeId,
        workerInterfaceId: request?.workerInterfaceId,
        doorHostSlots: request?.doorHostSlots,
        externalInputs: request?.externalInputs,
        externalRelays: request?.externalRelays,
        modelOutputs: request?.modelOutputs,
        contextArtifactBindings: request?.contextArtifactBindings,
        payloadArtifactBindings: authoredRequest?.payloadArtifactBindings,
        selectedWorkerEnvironmentKinds: request?.selectedWorkerEnvironmentKinds,
        selectedTools: request?.selectedTools,
        clientSurfaceRole,
        continuation: request?.continuation,
      });
      await validatePlannedHostBindings({ octopusHome, registryRoot: resolveRegistryRoot(options), plan });
      return {
        submission: {
          modelInterfaceId: plan.modelInterfaceId,
          workerInterfaceId: plan.workerInterfaceId,
          clientSurfaceRole: plan.clientSurfaceRole,
        },
        clientWrites: await commitClientWrites(plan.writes, options),
        outboundCursorOffset: plan.ledgerOffset,
      };
    }
    const existingChat = currentRunDocument.value;
    const identity = chatIdentityFromRunRecord(config, runId, existingChat);
    if (identity.chatId !== chatId || identity.interfaceId !== chatInterfaceId) {
      throw new Error(`operator chat run '${runId}' changed its exact client interface`);
    }
    const activeRun = existingRunSubmission(config, runId, existingChat, chatInterfaceId, {
      providerChoiceInterfaceId: providerReference,
      clientSurfaceRole,
      eeId: stringValue(request?.eeId),
      workerInterfaceId: stringValue(request?.workerInterfaceId),
    });
    if (activeRun) {
      const operatorMessageCheckpoint = activeOperatorMessageCheckpoint(
        existingChat,
        runId,
        chatInterfaceId,
        activeRun.modelInterfaceId,
        config.artifact_labels.message,
      );
      const conversationState = await readConversationState({
        octopusHome,
        existing: existingChat,
        runId,
        interfaceId: chatInterfaceId,
        terminalArtifactLabel: config.artifact_labels.terminal_output,
        errorArtifactLabel: config.artifact_labels.error_output,
        operatorMessageArtifactLabel: config.artifact_labels.message,
        operatorMessageCheckpoint,
        continuation,
      });
      const selection = await planExistingProviderSelection(options, {
        chatId,
        chatInterfaceId,
        clientSurfaceRole,
        conversationState,
        currentRunDocument,
        providerChoiceInterfaceId: providerReference,
        providerModel,
        reasoningEffort: request?.reasoningEffort,
        serviceTier: request?.serviceTier,
        payloadArtifactBindings: authoredRequest?.payloadArtifactBindings,
        preparedPayloadSelection,
        runId,
        authorNextCycle: conversationState.terminalAfterOperatorMessage,
        artifactRoot,
        messageId,
      });
      const revisionWrites = await applyOperatorInputRevision(options, {
        ...selection,
        artifactRoot,
        messageId,
        runId,
        chatInterfaceId,
        providerInterfaceId: activeRun.modelInterfaceId,
        timeoutMs,
      });
      const providerSelectionWrites = selection.writes;
      const clientDirectory = path.join(resolveRegistryRoot(options), "interface", chatInterfaceId);
      const changesClientConfiguration = (filePath) => path.dirname(filePath) === clientDirectory
        && path.extname(filePath) === ".json";
      const previousSocket = providerSelectionWrites.some((write) => changesClientConfiguration(write.filePath))
        ? await interfaceSocketIdentity(octopusHome, chatInterfaceId) : null;
      const clientWrites = [...revisionWrites, ...await commitClientWrites(providerSelectionWrites, options)];
      return {
        submission: activeRun,
        clientWrites,
        previousClientSocket: clientWrites.some((write) => write.changed && changesClientConfiguration(write.path))
          ? previousSocket : null,
        outboundCursorOffset: conversationState.ledgerOffset,
      };
    }
    throw new Error(`operator chat run '${runId}' has no active exact client input`);
  });
  const preparation = await (requestedRunId
    ? authorSubmission()
    : withOperatorWorkspaceTopologyLock(octopusHome, authorSubmission)).catch((error) => {
      error.deliveryState = "not_sent";
      throw error;
    });
  const { submission, clientWrites, outboundCursorOffset, previousClientSocket } = preparation;

  await waitForInterfaceSocket(octopusHome, chatInterfaceId, timeoutMs, previousClientSocket);
  const messageAdmission = await withOperatorMessageAdmissionLock(
    chatInterfaceId,
    messageId,
    () => admitOperatorMessage(options, {
      artifactRoot,
      chatInterfaceId,
      clientSurfaceRole: submission.clientSurfaceRole,
      label: config.artifact_labels.message,
      messageBytes: envelope.messageBytes,
      messageId,
      messagePath: envelope.messagePath,
      modelInterfaceId: submission.modelInterfaceId,
      octopusHome,
      outboundCursorOffset,
      runId,
      timeoutMs,
      workerInterfaceId: submission.workerInterfaceId,
    }),
  );
  return {
    runId,
    chatInterfaceId,
    modelInterfaceId: submission.modelInterfaceId,
    workerInterfaceId: submission.workerInterfaceId,
    clientSurfaceRole: submission.clientSurfaceRole,
    label: config.artifact_labels.message,
    messageId,
    payloadPath: envelope.messagePath,
    contentHash: sha256Hex(envelope.messageBytes),
    attachmentCount: envelope.payload.attachments.length,
    clientWrites,
    messageRecovered: messageAdmission.recovered,
    messageVerdict: messageAdmission.verdict,
    outboundCursor: {
      offset: messageAdmission.outboundCursorOffset,
      seq: null,
    },
  };
}

async function prepareOperatorPayloadArtifactBindings(options, input) {
  const requested = input.request?.payloadArtifactBindings === undefined
    ? undefined : operatorPayloadArtifactBindings(input.request.payloadArtifactBindings);
  let sidecar = null;
  let sidecarPath = null;
  let previous = [];
  if (input.existingRunDocument) {
    sidecarPath = path.join(resolveRegistryRoot(options), "interface", input.chatInterfaceId, "octopus_os.json");
    sidecar = await readJsonDocumentIfPresent(sidecarPath);
    const value = requiredObject(sidecar?.value, "operator payload authoring sidecar");
    const profile = requiredObject(value.authoring_profile, "operator payload authoring profile");
    const state = requiredObject(input.existingRunDocument.value.interface_state?.[input.chatInterfaceId], "operator payload client state");
    if (value.schema_version !== 1 || value.workspace !== "octopus_os" || value.component !== "client"
      || value.interface !== input.chatInterfaceId || value.owner_interface !== input.chatInterfaceId
      || value.surface_role !== input.clientSurfaceRole
      || profile.worker_choice_interface !== state.worker_choice_interface) {
      throw new Error("operator payload profile differs from its exact run, client or authoring owner");
    }
    const fields = new Set(["ee_id", "worker_choice_interface", "door_host_slots", "external_inputs", "external_relays",
      "model_outputs", "context_artifact_bindings", "payload_artifact_bindings", "selected_worker_environment_kinds",
      "selected_tools", "publication_evidence"]);
    if (Object.keys(profile).some((field) => !fields.has(field))) throw new Error("operator payload profile has undeclared fields");
    operatorDoorHostSlots(requiredObject(profile.door_host_slots, "authoring_profile.door_host_slots"));
    for (const field of ["external_inputs", "external_relays", "model_outputs", "context_artifact_bindings", "selected_tools", "publication_evidence"]) {
      if (Object.hasOwn(profile, field)) {
        for (const entry of requiredArray(profile[field], `authoring_profile.${field}`)) {
          requiredObject(entry, `authoring_profile.${field} entry`);
        }
      }
    }
    operatorContextArtifactBindings(profile.context_artifact_bindings);
    if (Object.hasOwn(profile, "selected_worker_environment_kinds")) {
      operatorSelectedWorkerEnvironmentKinds(requiredArray(profile.selected_worker_environment_kinds, "authoring_profile.selected_worker_environment_kinds"));
    }
    previous = operatorPayloadArtifactBindings(profile.payload_artifact_bindings);
  }
  const intentPath = path.join(input.artifactRoot, "message-intents", `${input.messageId}.json`);
  const saved = await readJsonDocumentIfPresent(intentPath);
  let bindings;
  if (saved) {
    if (saved.value.source_request_sha256 !== input.sourceRequestDigest
      || saved.value.run_id !== input.runId || saved.value.interface_id !== input.chatInterfaceId
      || saved.value.message_id !== input.messageId || !Object.hasOwn(saved.value, "payload_artifact_bindings")) {
      throw new Error("operator payload staging conflicts with its original durable intent");
    }
    bindings = operatorPayloadArtifactBindings(saved.value.payload_artifact_bindings);
  } else {
    const base = requested ?? previous;
    const proposed = await options.preparePayloadArtifactBindings({
      runId: input.runId, chatInterfaceId: input.chatInterfaceId, messageId: input.messageId,
      previousPayloadArtifactBindings: operatorPayloadArtifactBindings(previous),
      requestedPayloadArtifactBindings: requested === undefined ? undefined : operatorPayloadArtifactBindings(requested),
      payloadArtifactBindings: operatorPayloadArtifactBindings(base),
    });
    if (!Array.isArray(proposed)) throw new Error("payload preparation must return an explicit ArtifactRef array");
    bindings = operatorPayloadArtifactBindings(proposed);
    if (bindings.length < base.length || base.some((ref, index) => canonicalJson(ref) !== canonicalJson(bindings[index]))) {
      throw new Error("payload staging changed its explicitly selected base or an existing artifact content hash");
    }
  }
  const context = sidecar ? sidecar.value.authoring_profile.context_artifact_bindings : input.request?.contextArtifactBindings;
  const contextIds = new Set(operatorContextArtifactBindings(context).map((binding) => binding.id));
  if (bindings.some((reference) => contextIds.has(reference.id))) {
    throw new Error("payload staging collides with an explicit context artifact");
  }
  return {
    bindings, sidecar, sidecarPath, chatInterfaceId: input.chatInterfaceId,
    runScope: operatorPayloadRunScope(input.existingRunDocument, input.chatInterfaceId),
  };
}

function operatorPayloadRunScope(document, chatInterfaceId) {
  if (!document) return null;
  const run = requiredObject(document.value, "operator payload RunRecord");
  const state = requiredObject(run.interface_state, "operator payload interface state");
  const client = requiredObject(state[chatInterfaceId], "operator payload client state");
  const providerId = requiredString(client.provider_runtime_interface, "operator payload provider interface");
  const provider = requiredObject(state[providerId], "operator payload provider state");
  const declaration = Object.hasOwn(provider, "run_authoring_binding")
    ? { providerDeclaration: requiredObject(provider.run_authoring_binding, "operator payload input declaration") }
    : {};
  return canonicalJson({
    run: requiredTrimmedString(run.id, "operator payload run identity"),
    ee: requiredTrimmedString(run.ee, "operator payload EE"),
    worker: requiredTrimmedString(run.worker, "operator payload worker"),
    client,
    ...declaration,
  });
}

async function assertPreparedOperatorPayloadSelection(prepared, currentRunDocument) {
  if (operatorPayloadRunScope(currentRunDocument, prepared.chatInterfaceId) !== prepared.runScope) {
    throw new Error("operator payload staging RunRecord identity or input authority changed before authoring");
  }
  if (prepared.sidecar && await fs.readFile(prepared.sidecarPath, "utf8") !== prepared.sidecar.text) {
    throw new Error("operator payload staging private profile preimage changed before authoring");
  }
}

async function planExistingProviderSelection(options, input) {
  const config = await readConfig();
  const octopusHome = absolutePath(options.octopusHome(), "octopusHome");
  const registryRoot = resolveRegistryRoot(options);
  const eeInstancesRoot = await resolveEeInstancesRoot(octopusHome, options);
  const clientState = requiredObject(
    requiredObject(input.currentRunDocument.value.interface_state, "run.interface_state")[input.chatInterfaceId],
    `run.interface_state.${input.chatInterfaceId}`,
  );
  const providerInterfaceId = safeSegment(
    clientState.provider_runtime_interface,
    "provider runtime interface",
  );
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
  const privateConfig = requiredObject(
    requiredObject(
      providerDescriptor.value.interface_driver_config,
      "provider interface_driver_config",
    ).private_config,
    "provider private_config",
  );
  const currentModel = requiredString(privateConfig.model, "provider model");
  const currentServiceTier = stringValue(privateConfig.service_tier);
  const requestedServiceTier = optionalServiceTier(input.serviceTier);
  const currentReasoningEffort = stringValue(privateConfig.reasoning_effort);
  const requestedReasoningEffort = stringValue(input.reasoningEffort);
  const providerSelectionChanged = !(
    currentModel === input.providerModel
    && (!requestedServiceTier || currentServiceTier === requestedServiceTier)
    && (!requestedReasoningEffort || currentReasoningEffort === requestedReasoningEffort)
  );
  const sidecarPath = path.join(
    registryRoot,
    "interface",
    input.chatInterfaceId,
    "octopus_os.json",
  );
  const retainedPhases = await operatorInputRevisionPhases(input.artifactRoot, input.messageId);
  const retainedRevision = retainedPhases.definition || retainedPhases.rearm;
  const providerState = requiredObject(input.currentRunDocument.value.interface_state[providerInterfaceId], "provider authoring state");
  const initialInput = input.currentRunDocument.value.status === "idle"
    && providerState.pending_handoff === undefined
    && input.conversationState.operatorMessageCheckpointAdmitted === false;
  const unchanged = { writes: [], desiredInputs: null, rearm: false };
  if (!providerSelectionChanged && !input.authorNextCycle && input.payloadArtifactBindings === undefined && !retainedRevision && !initialInput) return unchanged;
  if (input.preparedPayloadSelection && input.preparedPayloadSelection.sidecarPath !== sidecarPath) {
    throw new Error("operator payload staging changed its original registry owner path");
  }
  const sidecar = input.preparedPayloadSelection
    ? input.preparedPayloadSelection.sidecar : await readJsonDocumentIfPresent(sidecarPath);
  const authoringProfile = requiredObject(
    sidecar?.value?.authoring_profile,
    `operator chat sidecar '${input.chatInterfaceId}' authoring_profile`,
  );
  const previousPayloadBindings = operatorPayloadArtifactBindings(authoringProfile.payload_artifact_bindings);
  if (!retainedPhases.rearm) {
    const inspected = await runOperatorRevisionCommand(resolvePackageExecutable(
      config, resolveInstallationRoot(options), registryRoot, "topology_planner",
    ), "--inspect-input-payloads", {
      run_record_content: input.currentRunDocument.text, provider: providerInterfaceId,
    });
    const selected = operatorPayloadArtifactBindings(requiredArray(inspected.selected_payloads, "selected_payloads"));
    if (canonicalJson(selected) !== canonicalJson(previousPayloadBindings)) {
      throw new Error("operator payload profile awaits publication of its installed input selection");
    }
    if (input.preparedPayloadSelection) {
      await assertPreparedOperatorPayloadSelection(input.preparedPayloadSelection,
        await readJsonDocumentIfPresent(path.join(await resolveRunRecordsRoot(octopusHome), `${input.runId}.json`)));
    }
  }
  const payloadArtifactBindings = input.payloadArtifactBindings === undefined
    ? previousPayloadBindings
    : operatorPayloadArtifactBindings(input.payloadArtifactBindings);
  const payloadSelectionChanged = canonicalJson(previousPayloadBindings) !== canonicalJson(payloadArtifactBindings);
  let authorNextCycle = input.authorNextCycle;
  if (payloadSelectionChanged) {
    assertPayloadInputCycleReady(input.currentRunDocument.value, providerInterfaceId, input.conversationState);
    authorNextCycle = true;
  }
  if (!providerSelectionChanged && !authorNextCycle && !retainedRevision && !initialInput) return unchanged;
  const topologyServiceTier = requestedServiceTier || currentServiceTier;
  const topologyReasoningEffort = !providerSelectionChanged
    ? currentReasoningEffort
    : requestedReasoningEffort;
  const expectedProfile = { ...authoringProfile };
  if (payloadArtifactBindings.length) expectedProfile.payload_artifact_bindings = payloadArtifactBindings;
  else delete expectedProfile.payload_artifact_bindings;
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
    chatId: input.chatId,
    providerInterfaceId: input.providerChoiceInterfaceId,
    providerModel: input.providerModel,
    reasoningEffort: topologyReasoningEffort || null,
    serviceTier: topologyServiceTier,
    eeId: safeSegment(input.currentRunDocument.value.ee, "run.ee"),
    workerInterfaceId: safeSegment(
      clientState.worker_choice_interface,
      "client worker choice",
    ),
    doorHostSlots: requiredObject(authoringProfile.door_host_slots, "authoring_profile.door_host_slots"),
    externalInputs: arrayValue(authoringProfile.external_inputs),
    externalRelays: arrayValue(authoringProfile.external_relays),
    modelOutputs: arrayValue(authoringProfile.model_outputs),
    contextArtifactBindings: arrayValue(authoringProfile.context_artifact_bindings),
    payloadArtifactBindings,
    selectedWorkerEnvironmentKinds: arrayValue(authoringProfile.selected_worker_environment_kinds),
    selectedTools: arrayValue(authoringProfile.selected_tools),
    clientSurfaceRole: input.clientSurfaceRole,
    cycle: randomToken(24),
    existingRunRecord: input.currentRunDocument.value,
    retainCurrentCapabilities: true,
    conversation: {
      ledger_offset: input.conversationState.ledgerOffset,
      artifact_bindings: input.conversationState.artifactBindings,
      pinned_artifact_bindings: input.conversationState.pinnedArtifactBindings,
      pinned_context_items: input.conversationState.pinnedContextItems,
      continued_from: input.conversationState.continuedFrom,
    },
  });
  if (
    topologyPlan.expected_before_hash_algorithm !== "sha256"
    || topologyPlan.run_id !== input.runId
    || topologyPlan.chat_interface_id !== input.chatInterfaceId
    || topologyPlan.model_interface_id !== providerInterfaceId
    || topologyPlan.model !== input.providerModel
    || (topologyReasoningEffort && topologyPlan.reasoning_effort !== topologyReasoningEffort)
    || (topologyServiceTier && topologyPlan.service_tier !== topologyServiceTier)
  ) {
    throw new Error("operator chat provider selection changed its exact task identity");
  }
  assertPublicationExpansion(topologyPlan, expectedProfile, []);
  const removals = requiredArray(topologyPlan.removals, "plan.removals");
  if (removals.length > 0) {
    throw new Error("operator chat provider selection unexpectedly removes private capabilities");
  }
  const writes = topologyPlan.interface_writes.map((write, index) => (
    plannedClientWrite(write, index, registryRoot)
  ));
  const providerWrite = writes.find((write) => write.filePath === providerDescriptorPath);
  if (providerSelectionChanged && !providerWrite) {
    throw new Error("operator chat provider selection produced no exact provider write");
  }
  if (plannedAdditionalRunWrites(topologyPlan, await resolveRunRecordsRoot(octopusHome), registryRoot).length > 0) {
    throw new Error("operator input revision cannot create undeclared replacement writer Runs");
  }
  const selectedWrites = [];
  if (providerWrite && providerSelectionChanged) {
    const providerDescriptorHash = sha256Hex(Buffer.from(providerDescriptor.text));
    if (providerWrite.expectedBeforeHash !== providerDescriptorHash) {
      throw new Error("operator chat provider selection did not preserve its exact descriptor revision");
    }
    providerWrite.content = selectedProviderDescriptorContent(
      providerDescriptor.text,
      providerWrite.content,
      providerInterfaceId,
      topologyPlan.model,
      topologyPlan.reasoning_effort,
      topologyPlan.service_tier,
    );
    if (providerDescriptor.text !== providerWrite.content) selectedWrites.push(providerWrite);
  }
  let desiredInputs = null;
  if (authorNextCycle || retainedRevision || initialInput) {
    const profileWrite = writes.find((write) => write.filePath === sidecarPath);
    if (payloadSelectionChanged) {
      if (!profileWrite || profileWrite.expectedBeforeHash !== sha256Hex(Buffer.from(sidecar.text))) {
        throw new Error("next-cycle payload selection has no exact checked profile write");
      }
      const nextSidecar = JSON.parse(profileWrite.content);
      const expectedSidecar = { ...sidecar.value, authoring_profile: expectedProfile };
      if (canonicalJson(nextSidecar) !== canonicalJson(expectedSidecar)) {
        throw new Error("next-cycle payload selection changed unrelated client sidecar data");
      }
      selectedWrites.push(profileWrite);
    } else if (profileWrite) {
      throw new Error("next-cycle selection unexpectedly changes the client profile");
    }
    const unexpectedWrites = writes.filter((write) => write !== providerWrite && write !== profileWrite);
    if (unexpectedWrites.length > 0) {
      throw new Error("operator chat next cycle requires repair of changed private capabilities");
    }
    const plannedRun = validatePlannedRunRecord(
      topologyPlan.run_record_content,
      input.runId,
      input.chatInterfaceId,
      safeSegment(input.currentRunDocument.value.ee, "run.ee"),
      safeSegment(input.currentRunDocument.value.worker, "run.worker"),
      input.clientSurfaceRole,
    );
    desiredInputs = requiredArray(plannedRun.interface_state?.[providerInterfaceId]?.run_authoring_binding?.checkpoints,
      "fresh planner provider input templates").filter((template) => template.role === "owner_input");
  }
  return { writes: selectedWrites, desiredInputs, rearm: authorNextCycle || retainedPhases.rearm,
    payloadArtifactBindings, payloadSelectionChanged };
}

async function operatorInputRevisionPhases(artifactRoot, messageId) {
  const phases = {};
  for (const phase of ["definition", "rearm"]) {
    phases[phase] = await fileExists(path.join(artifactRoot, "input-revisions", `${messageId}-${phase}.artifact.json`));
  }
  return phases;
}

async function applyOperatorInputRevision(options, input) {
  if (input.desiredInputs === null && !input.rearm) return [];
  const config = await readConfig();
  const octopusHome = absolutePath(options.octopusHome(), "octopusHome");
  const registryRoot = resolveRegistryRoot(options);
  const planner = resolvePackageExecutable(config, resolveInstallationRoot(options), registryRoot, "topology_planner");
  const runPath = path.join(await resolveRunRecordsRoot(octopusHome), `${input.runId}.json`);
  const scope = { registry_root: registryRoot, run_path: runPath,
    ledger_root: path.join(octopusHome, "ledger"), artifact_root: input.artifactRoot,
    interface_hand_in_root: path.join(octopusHome, "interface-hand-in"),
    provider: input.providerInterfaceId, source: input.chatInterfaceId };
  const operations = [];
  if (input.desiredInputs !== null) operations.push(["definition", { kind: "owner_inputs", desired_inputs: input.desiredInputs }]);
  if (input.rearm) operations.push(["rearm", input.payloadSelectionChanged
    ? { kind: "payloads", selected: input.payloadArtifactBindings } : { kind: "rearm" }]);
  const writes = [];
  const deadline = Date.now() + input.timeoutMs;
  for (const [phase, operation] of operations) {
    const request = { ...scope, key: `${input.messageId}-${phase}` };
    const retained = await fileExists(path.join(input.artifactRoot, "input-revisions", `${request.key}.artifact.json`));
    const prepared = retained ? { status: "prepared" }
      : await runOperatorRevisionCommand(planner, "--prepare-input-revision", {
        ...request, operation, expected_before_content: await fs.readFile(runPath, "utf8"),
      });
    request.selected_payloads = phase === "rearm" ? input.payloadArtifactBindings : null;
    if (prepared.status === "unchanged") continue;
    if (prepared.status === "deferred") throw operatorRevisionDeferred(prepared);
    if (prepared.status !== "prepared") throw new Error("operator input revision returned an unknown preparation state");
    let sent = false;
    let sendError = null;
    for (;;) {
      const remaining = deadline - Date.now();
      const change = watchLedgerChange(path.join(scope.ledger_root, "movements.jsonl"), Math.max(1, remaining));
      try {
        const checked = await runOperatorRevisionCommand(planner, "--verify-input-revision", request);
        if (checked.status === "installed") {
          if (checked.path !== runPath) throw new Error("checked revision changed its exact Run path");
          requiredObject(checked.install, "checked revision installation");
          writes.push({ path: runPath, tag: `run:${input.runId}:${phase}`, changed: true,
            hashAlgorithm: "blake3", beforeHash: requiredString(checked.before_hash, "checked revision before hash"),
            afterHash: requiredString(checked.after_hash, "checked revision after hash"), install: checked.install });
          break;
        }
        if (checked.status === "deferred") throw operatorRevisionDeferred(checked);
        if (!["ready", "awaiting_result"].includes(checked.status)) throw new Error("operator input revision returned an unknown proof state");
        if (remaining <= 0) throw new Error(`operator input revision '${request.key}' awaits its checked writer result; the retained call will be recovered on retry`);
        if (checked.status === "ready" && !sent) {
          const source = safeSegment(checked.source_interface, "checked writer source");
          const manifest = requiredObject(checked.manifest, "checked writer manifest");
          await waitForInterfaceSocket(octopusHome, source, Math.max(1, deadline - Date.now()));
          sent = true;
          try {
            const verdict = await requiredFunction(options.handInArtifact, "handInArtifact")({
              endpoint: interfaceEndpoint(octopusHome, source), interfaceId: source,
              manifest, timeoutMs: Math.max(1, deadline - Date.now()),
            });
            assertAdmitted(verdict, requiredString(manifest.label, "checked writer artifact label"));
          } catch (error) { sendError = error; }
          continue;
        }
        if (checked.status === "ready" && sendError) throw sendError;
        await change.promise;
      } finally { change.cancel(); }
    }
  }
  return writes;
}

function operatorRevisionDeferred(proof) {
  const error = new Error("The task's accepted work must finish before its next input contract can be updated");
  error.code = "operator_input_revision_deferred";
  error.checkpoints = proof.checkpoints ?? [];
  return error;
}

async function runOperatorRevisionCommand(planner, flag, request, maximumResponseBytes = 4 * 1024 * 1024) {
  const output = await new Promise((resolve, reject) => {
    const child = spawn(planner, [flag], { stdio: ["pipe", "pipe", "pipe"] });
    const stdout = [], stderr = [];
    let size = 0;
    child.stdout.on("data", (chunk) => {
      size += chunk.length;
      if (size > maximumResponseBytes) { child.kill(); reject(new Error("operator input revision response exceeds its bound")); }
      else stdout.push(chunk);
    });
    child.stderr.on("data", (chunk) => { if (Buffer.concat(stderr).length < 256 * 1024) stderr.push(chunk); });
    child.on("error", reject);
    child.on("close", (code) => code === 0 ? resolve(Buffer.concat(stdout).toString("utf8"))
      : reject(new Error(Buffer.concat(stderr).toString("utf8").trim() || `operator input revision exited with code ${code}`)));
    child.stdin.on("error", reject);
    child.stdin.end(JSON.stringify(request));
  });
  return requiredObject(JSON.parse(output), "operator input revision proof");
}

async function loadOperatorTaskLifecycle(options) {
  const config = await readConfig();
  const registryRoot = resolveRegistryRoot(options);
  const planner = resolvePackageExecutable(config, resolveInstallationRoot(options), registryRoot, "topology_planner");
  return {
    taskLifecycle: require("../maintenance-lifecycle.cjs"),
    retirementBarrier: require("../task-retirement.cjs").createTaskRetirementBarrier(planner, runOperatorRevisionCommand),
  };
}

function assertPayloadInputCycleReady(record, providerInterfaceId, conversation) {
  if (record.status !== "idle"
    || typeof conversation.operatorMessageCheckpointAdmitted !== "boolean"
    || typeof conversation.terminalAfterOperatorMessage !== "boolean"
    || (conversation.operatorMessageCheckpointAdmitted && !conversation.terminalAfterOperatorMessage)) {
    throw new Error("Attachments can change after the task's current input has settled");
  }
  const state = requiredObject(record.interface_state?.[providerInterfaceId], "provider authoring state");
  nonNegativeInteger(state.authoring_generation, "provider authoring_generation");
  if (state.pending_handoff === undefined) {
    if (conversation.operatorMessageCheckpointAdmitted) throw new Error("Attachment selection has no completed provider handoff evidence");
    return;
  }
  const pending = requiredObject(state.pending_handoff, "provider pending_handoff");
  if (pending.terminal !== true || (pending.context_publication !== undefined && pending.context_publication !== null)) {
    throw new Error("Attachments can change after the provider has published its completed context");
  }
}

function selectedProviderDescriptorContent(
  currentText,
  plannedText,
  providerInterfaceId,
  model,
  reasoningEffort,
  serviceTier,
) {
  const plannedProviderDescriptor = requiredObject(
    JSON.parse(plannedText),
    "planned provider descriptor",
  );
  const selectedProviderDescriptor = requiredObject(
    JSON.parse(currentText),
    "selected provider descriptor",
  );
  if (
    plannedProviderDescriptor.id !== providerInterfaceId
    || selectedProviderDescriptor.id !== providerInterfaceId
  ) {
    throw new Error("operator chat provider selection planned a different private provider identity");
  }
  selectedProviderDescriptor.interface_policy = requiredObject(
    plannedProviderDescriptor.interface_policy,
    "planned provider interface_policy",
  );
  const selectedProviderDriverConfig = requiredObject(
    selectedProviderDescriptor.interface_driver_config,
    "selected provider interface_driver_config",
  );
  const plannedProviderDriverConfig = requiredObject(
    plannedProviderDescriptor.interface_driver_config,
    "planned provider interface_driver_config",
  );
  selectedProviderDriverConfig.supported_reasoning_efforts = requiredArray(
    plannedProviderDriverConfig.supported_reasoning_efforts,
    "planned provider supported_reasoning_efforts",
  );
  const selectedProviderPrivateConfig = requiredObject(
    selectedProviderDriverConfig.private_config,
    "selected provider private_config",
  );
  const plannedProviderPrivateConfig = requiredObject(
    plannedProviderDriverConfig.private_config,
    "planned provider private_config",
  );
  selectedProviderPrivateConfig.model_catalog = requiredArray(
    plannedProviderPrivateConfig.model_catalog,
    "planned provider model_catalog",
  );
  selectedProviderPrivateConfig.model = requiredString(model, "planned provider model");
  const resolvedServiceTier = optionalServiceTier(serviceTier);
  if (resolvedServiceTier) selectedProviderPrivateConfig.service_tier = resolvedServiceTier;
  else delete selectedProviderPrivateConfig.service_tier;
  const resolvedReasoningEffort = stringValue(reasoningEffort);
  if (resolvedReasoningEffort) {
    selectedProviderPrivateConfig.reasoning_effort = resolvedReasoningEffort;
  } else {
    delete selectedProviderPrivateConfig.reasoning_effort;
  }
  return `${JSON.stringify(selectedProviderDescriptor, null, 2)}\n`;
}
