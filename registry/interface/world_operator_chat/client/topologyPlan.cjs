module.exports = { assertPublicationExpansion, canonicalJson, interfaceEndpoint, interfaceSocketIdentity, interfaceSocketPath, operatorContextArtifactBindings, operatorDoorHostSlots, operatorExternalInputs, operatorExternalRelays, operatorModelOutputs, operatorPayloadArtifactBindings, operatorPublications, operatorRequiredVerifiedArtifacts, operatorSelectedTools, operatorSelectedWorkerEnvironmentKinds, operatorSurfaceRole, pathIsWithin, planOperatorChatTopology, plannedAdditionalRunWrites, plannedArtifactOwner, plannedClientRemoval, plannedClientWrite, plannedExactIds, safeContractToken, safeSha256, validatePlannedRunRecord };

const { spawn } = require("node:child_process");
const crypto = require("node:crypto");
const fs = require("node:fs/promises");
const path = require("node:path");
const { plannedMaintenanceRuns } = require("../maintenance-client.cjs");
const { resolveRunRecordsRoot } = require("./operatorMessages.cjs");
const { fileExists } = require("./publication.cjs");
const { resolveEeInstancesRoot } = require("./roots.cjs");
const { absolutePath, isObject, optionalServiceTier, requiredArray, requiredObject, requiredString, requiredTrimmedString, safeSegment, stringValue } = require("./values.cjs");

async function planOperatorChatTopology({
  octopusHome,
  topologyPlanner,
  registryRoot,
  eeInstancesRoot,
  chatId,
  providerInterfaceId,
  providerModel,
  reasoningEffort,
  serviceTier,
  voiceModel,
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
  existingRunRecord,
  retainCurrentCapabilities = false,
  conversation,
}) {
  if (!(await fileExists(topologyPlanner))) {
    throw new Error(
      `operator chat topology planner is absent at its declared package path '${topologyPlanner}'`,
    );
  }
  const request = JSON.stringify({
    registry_root: registryRoot,
    ee_instances_root: await resolveEeInstancesRoot(octopusHome),
    ee_policy_root: eeInstancesRoot,
    run_records_root: await resolveRunRecordsRoot(octopusHome),
    write_hash_algorithm: "sha256",
    interface_private_state_root: path.join(octopusHome, "interfaces"),
    chat_id: chatId,
    provider_interface_id: providerInterfaceId,
    model: stringValue(providerModel) || null,
    reasoning_effort: stringValue(reasoningEffort) || null,
    service_tier: optionalServiceTier(serviceTier) || null,
    ...(stringValue(voiceModel) ? { voice_model: stringValue(voiceModel) } : {}),
    ee_id: stringValue(eeId) || null,
    worker_interface_id: stringValue(workerInterfaceId) || null,
    client_surface_role: clientSurfaceRole,
    cycle,
    door_host_slots: doorHostSlots,
    external_inputs: externalInputs,
    external_relays: externalRelays,
    model_outputs: modelOutputs,
    context_artifact_bindings: contextArtifactBindings,
    payload_artifact_bindings: operatorPayloadArtifactBindings(payloadArtifactBindings),
    publications: operatorPublications(publications),
    selected_worker_environment_kinds: selectedWorkerEnvironmentKinds,
    selected_tools: selectedTools.map((selection) => ({
      tool: selection.tool,
      interface: selection.interface,
      worker_environment_kind: selection.worker_environment_kind,
    })),
    existing_run_record: existingRunRecord ?? null,
    retain_current_capabilities: retainCurrentCapabilities,
    conversation,
  });
  const stdout = await new Promise((resolve, reject) => {
    const child = spawn(topologyPlanner, [], { stdio: ["pipe", "pipe", "pipe"] });
    const out = [];
    const err = [];
    child.stdout.on("data", (chunk) => out.push(chunk));
    child.stderr.on("data", (chunk) => err.push(chunk));
    child.on("error", reject);
    child.on("close", (code) => {
      if (code === 0) {
        resolve(Buffer.concat(out).toString("utf8"));
        return;
      }
      reject(new Error(
        Buffer.concat(err).toString("utf8").trim()
          || `operator chat topology planner exited with code ${code}`,
      ));
    });
    child.stdin.on("error", reject);
    child.stdin.end(request);
  });
  const plan = JSON.parse(stdout);
  if (!isObject(plan)) throw new Error("operator chat topology planner returned a non-object plan");
  requiredString(plan.run_id, "plan.run_id");
  requiredString(plan.chat_interface_id, "plan.chat_interface_id");
  requiredString(plan.model_interface_id, "plan.model_interface_id");
  requiredString(plan.worker_interface_id, "plan.worker_interface_id");
  requiredObject(plan.authoring_profile, "plan.authoring_profile");
  requiredArray(plan.required_interface_ids, "plan.required_interface_ids");
  requiredArray(plan.required_tool_ids, "plan.required_tool_ids");
  requiredArray(plan.interface_writes, "plan.interface_writes");
  requiredString(plan.run_record_content, "plan.run_record_content");
  return plan;
}

function plannedExactIds(value, field, validate) {
  const values = requiredArray(value, field).map((item, index) => (
    validate(item, `${field}[${index}]`)
  ));
  if (values.length === 0) throw new Error(`${field} must not be empty`);
  if (new Set(values).size !== values.length) throw new Error(`${field} repeats an exact id`);
  const sorted = [...values].sort();
  if (values.some((item, index) => item !== sorted[index])) {
    throw new Error(`${field} must be sorted`);
  }
  return values;
}

function plannedAdditionalRunWrites(plan, runsRoot, registryRoot) {
  const target = requiredObject(JSON.parse(plan.run_record_content), "planned target Run");
  const proposed = requiredArray(plan.run_writes, "plan.run_writes").map((write, index) => {
    requiredObject(write, `plan.run_writes[${index}]`);
    if (Object.keys(write).some((key) => !["path", "content", "tag", "expected_before_hash"].includes(key))) {
      throw new Error("planned additional Run contains unknown fields");
    }
    if (typeof write.content !== "string" || !write.content.trim()) throw new Error("additional Run content is required");
    const content = write.content;
    const record = requiredObject(JSON.parse(content), "additional Run record");
    const id = safeSegment(record.id, "additional Run id");
    const filePath = absolutePath(write.path, "additional Run path");
    if (id === target.id || filePath !== path.join(runsRoot, `${id}.json`)
      || write.tag !== `run:${id}` || write.expected_before_hash !== null) {
      throw new Error("additional Run write must create its exact declared path");
    }
    return { record, write: { filePath, content, tag: write.tag, expectedBeforeHash: null } };
  });
  if (new Set(proposed.map(({record}) => record.id)).size !== proposed.length) {
    throw new Error("additional Run writes repeat an identity");
  }
  if (!proposed.length) return [];
  const declared = new Map();
  const exactContents = new Map();
  const execution = [target];
  const context = target.interface_state?.[plan.chat_interface_id]?.voice_context;
  const voice = proposed.find(({record}) => record.id === context?.run_id)?.record;
  if (voice) {
    const origin = voice.interface_state?.[context.control_interface_id];
    const topology = voice.interface_state?.[context.provider_id]?.run_authoring_binding;
    if (voice.worker !== target.worker
      || origin?.home_run !== target.id || origin?.home_interface !== plan.chat_interface_id
      || topology?.run !== voice.id || topology?.worker !== voice.worker
      || topology?.provider_interface !== context.provider_id
      || target.interface_state?.[context.provider_id]
      || !topology?.checkpoints?.some((cp) => cp.role === "provider_interface_output"
        && cp.outbound_interface_bindings?.some((binding) => binding.interface === context.delegation_interface_id))) {
      throw new Error("voice Run differs from its exact independent Home binding");
    }
    declared.set(voice.id, voice);
    execution.push(voice);
  }
  const proposedIds = new Set(proposed.map(({record}) => record.id));
  const maintenance = target.interface_state?.[plan.model_interface_id]?.maintenance_observer;
  const createsMaintenance = [maintenance?.run, maintenance?.projection_run?.run].some((id) => proposedIds.has(id));
  const companions = plannedMaintenanceRuns(plan, target, registryRoot, createsMaintenance);
  const companionIds = new Set(companions.map(({record}) => record.id));
  const requiredCompanions = new Set();
  for (const { record, content } of companions) {
    if (declared.has(record.id) || record.id === target.id) throw new Error("maintenance Run repeats another execution owner");
    declared.set(record.id, record);
    exactContents.set(record.id, content);
    execution.push(record);
    if (createsMaintenance) requiredCompanions.add(record.id);
  }
  for (const target of execution) {
    const states = requiredObject(target.interface_state, "planned target state");
    for (const [owner, state] of Object.entries(states)) {
      const writer = state.run_authoring_binding?.writer_run ?? state.writer_run;
      if (!writer) continue;
      const id = safeSegment(writer.run, "declared writer Run");
      if (declared.has(id) || execution.some((run) => run.id === id)) throw new Error("two owners declare the same writer Run");
      if (Object.hasOwn(writer, "initial_record_format") && !["pretty", "compact"].includes(writer.initial_record_format)) {
        throw new Error("writer Run declaration has invalid initial record format");
      }
      const record = requiredObject(writer.initial_record, "declared writer initial record");
      if (record.id !== id || record.worker !== target.worker
        || record.interface_state?.[owner]?.file_writer_target?.run !== target.id) {
        throw new Error("writer Run declaration changes its target or worker");
      }
      declared.set(id, record);
      if (createsMaintenance && companionIds.has(target.id)) requiredCompanions.add(id);
    }
  }
  if ([...requiredCompanions].some((id) => !proposedIds.has(id))) {
    throw new Error("maintenance creation must include both companions and their exact declared writers");
  }
  return proposed.map(({record, write}) => {
    if (!declared.has(record.id) || canonicalJson(record) !== canonicalJson(declared.get(record.id))
      || exactContents.has(record.id) && write.content !== exactContents.get(record.id)) {
      throw new Error("planned write differs from its exact declared initial Run");
    }
    declared.delete(record.id);
    return write;
  });
}

function plannedClientWrite(value, index, registryRoot) {
  const write = requiredObject(value, `plan.interface_writes[${index}]`);
  const unknown = Object.keys(write).filter((field) => ![
    "path",
    "content",
    "tag",
    "expected_before_hash",
  ].includes(field));
  if (unknown.length > 0) {
    throw new Error(`plan.interface_writes[${index}] contains unknown fields: ${unknown.join(", ")}`);
  }
  const filePath = absolutePath(write.path, `plan.interface_writes[${index}].path`);
  const descriptorRoots = [
    path.join(registryRoot, "interface"),
    path.join(registryRoot, "tool"),
  ];
  const artifactRoot = path.join(registryRoot, "artifact");
  const artifactParts = path.relative(artifactRoot, filePath).split(path.sep);
  const isArtifactDescriptor = artifactParts.length === 2
    && artifactParts[1] === "artifact.json"
    && pathIsWithin(artifactRoot, filePath);
  if (isArtifactDescriptor) {
    const id = plannedArtifactOwner(artifactParts[0]);
    const descriptor = requiredObject(JSON.parse(write.content), "planned artifact descriptor");
    if (descriptor.id !== id || write.tag !== `artifact:${id}`) {
      throw new Error("planned artifact descriptor identity differs from its exact owner");
    }
  }
  if (!isArtifactDescriptor && !descriptorRoots.some((root) => pathIsWithin(root, filePath))) {
    throw new Error(`plan.interface_writes[${index}].path escapes the interface/tool/artifact registries`);
  }
  if (typeof write.content !== "string" || write.content.length === 0) {
    throw new Error(`plan.interface_writes[${index}].content must be a non-empty string`);
  }
  const expectedBeforeHash = write.expected_before_hash;
  if (expectedBeforeHash !== null
    && (typeof expectedBeforeHash !== "string" || !/^[0-9a-f]{64}$/.test(expectedBeforeHash))) {
    throw new Error(
      `plan.interface_writes[${index}].expected_before_hash must be a lowercase SHA-256 hash or null`,
    );
  }
  return {
    filePath,
    content: write.content,
    tag: requiredString(write.tag, `plan.interface_writes[${index}].tag`),
    expectedBeforeHash,
  };
}

function plannedArtifactOwner(value) {
  const id = requiredTrimmedString(value, "planned artifact owner");
  if (id === "." || id === ".." || /[/\\\0]/.test(id)) {
    throw new Error("planned artifact owner must be one exact registry path component");
  }
  return id;
}

function plannedClientRemoval(value, index, registryRoot) {
  const removal = requiredObject(value, `plan.removals[${index}]`);
  const unknown = Object.keys(removal).filter((field) => !["path", "tag", "files"].includes(field));
  if (unknown.length > 0) {
    throw new Error(`plan.removals[${index}] contains unknown fields: ${unknown.join(", ")}`);
  }
  const directory = absolutePath(removal.path, `plan.removals[${index}].path`);
  const roots = [path.join(registryRoot, "interface"), path.join(registryRoot, "tool")];
  if (!roots.some((root) => pathIsWithin(root, directory))) {
    throw new Error(`plan.removals[${index}].path escapes the interface/tool/artifact registries`);
  }
  const files = requiredArray(removal.files, `plan.removals[${index}].files`).map((candidate, fileIndex) => {
    const file = requiredObject(candidate, `plan.removals[${index}].files[${fileIndex}]`);
    const relativePath = requiredString(
      file.relative_path,
      `plan.removals[${index}].files[${fileIndex}].relative_path`,
    );
    if (path.basename(relativePath) !== relativePath || relativePath.includes("\0")) {
      throw new Error(`plan.removals[${index}] contains a non-local file path`);
    }
    return {
      relativePath,
      sha256: safeSha256(file.sha256, `plan.removals[${index}].files[${fileIndex}].sha256`),
    };
  });
  if (files.length === 0 || new Set(files.map((file) => file.relativePath)).size !== files.length) {
    throw new Error(`plan.removals[${index}] must name unique files`);
  }
  return {
    directory,
    tag: requiredString(removal.tag, `plan.removals[${index}].tag`),
    files,
  };
}

function safeSha256(value, field) {
  const hash = requiredString(value, field);
  if (!/^[0-9a-f]{64}$/.test(hash)) throw new Error(`${field} must be a lowercase SHA-256 hash`);
  return hash;
}

function pathIsWithin(root, candidate) {
  const relative = path.relative(path.resolve(root), path.resolve(candidate));
  return relative !== ""
    && relative !== ".."
    && !relative.startsWith(`..${path.sep}`)
    && !path.isAbsolute(relative);
}

function validatePlannedRunRecord(
  content,
  runId,
  chatInterfaceId,
  eeId,
  workerInterfaceId,
  clientSurfaceRole,
) {
  let record;
  try {
    record = JSON.parse(content);
  } catch (error) {
    throw new Error(`plan.run_record_content is malformed JSON: ${error.message}`);
  }
  requiredObject(record, "plan run record");
  if (requiredString(record.id, "plan run record.id") !== runId) {
    throw new Error("topology plan run record changed its run identity");
  }
  if (requiredString(record.ee, "plan run record.ee") !== eeId) {
    throw new Error("topology plan run record changed its EE identity");
  }
  if (requiredString(record.worker, "plan run record.worker") !== workerInterfaceId) {
    throw new Error("topology plan run record changed its worker identity");
  }
  const interfaceState = requiredObject(record.interface_state, "plan run record.interface_state");
  const clientState = requiredObject(
    interfaceState[chatInterfaceId],
    `plan run record.interface_state.${chatInterfaceId}`,
  );
  if (clientState.client_surface_role !== clientSurfaceRole) {
    throw new Error("topology plan run record changed its client surface role");
  }
  return record;
}

function canonicalJson(value) {
  if (Array.isArray(value)) return `[${value.map(canonicalJson).join(",")}]`;
  if (isObject(value)) {
    return `{${Object.keys(value).sort().map((key) => (
      `${JSON.stringify(key)}:${canonicalJson(value[key])}`
    )).join(",")}}`;
  }
  return JSON.stringify(value);
}

function operatorSurfaceRole(value) {
  const role = requiredString(value, "clientSurfaceRole");
  if (role !== "home" && role !== "task") {
    throw new Error("clientSurfaceRole must be 'home' or 'task'");
  }
  return role;
}

function operatorDoorHostSlots(value) {
  if (value === undefined || value === null) return {};
  if (!isObject(value)) throw new Error("doorHostSlots must be a JSON object");
  const result = {};
  for (const [interfaceId, slots] of Object.entries(value)) {
    const interfaceKey = safeSegment(interfaceId, "doorHostSlots interface");
    if (!isObject(slots)) {
      throw new Error(`doorHostSlots.${interfaceKey} must be a JSON object`);
    }
    const configured = {};
    for (const [slot, root] of Object.entries(slots)) {
      const slotKey = safeSegment(slot, `doorHostSlots.${interfaceKey} slot`);
      configured[slotKey] = absolutePath(
        root,
        `doorHostSlots.${interfaceKey}.${slotKey}`,
      );
    }
    result[interfaceKey] = configured;
  }
  return result;
}

function operatorExternalInputs(value) {
  if (value === undefined || value === null) return [];
  if (!Array.isArray(value)) throw new Error("externalInputs must be an array");
  const checkpoints = new Set();
  const interfaces = new Set();
  return value.map((input, index) => {
    if (!isObject(input)) throw new Error(`externalInputs[${index}] must be a JSON object`);
    const checkpointId = safeSegment(input.checkpointId, `externalInputs[${index}].checkpointId`);
    const interfaceId = safeSegment(input.interfaceId, `externalInputs[${index}].interfaceId`);
    const artifactNames = operatorArtifactNames(input.artifactNames, `externalInputs[${index}].artifactNames`);
    const requiredVerifiedArtifacts = operatorRequiredVerifiedArtifacts(
      input.requiredVerifiedArtifacts,
      `externalInputs[${index}].requiredVerifiedArtifacts`,
    );
    const rawAllowedOperations = input.allowedOperations !== undefined
      ? input.allowedOperations
      : input.allowed_operations;
    const allowedOperations = operatorAllowedOperations(
      rawAllowedOperations,
      `externalInputs[${index}].allowedOperations`,
    );
    if (checkpoints.has(checkpointId)) throw new Error(`externalInputs repeats checkpoint '${checkpointId}'`);
    if (interfaces.has(interfaceId)) throw new Error(`externalInputs repeats interface '${interfaceId}'`);
    checkpoints.add(checkpointId);
    interfaces.add(interfaceId);
    return {
      checkpoint_id: checkpointId,
      interface_id: interfaceId,
      ...(artifactNames.length > 0 ? { artifact_names: artifactNames } : {}),
      ...(requiredVerifiedArtifacts.length > 0
        ? { required_verified_artifacts: requiredVerifiedArtifacts }
        : {}),
      ...(allowedOperations !== null ? { allowed_operations: allowedOperations } : {}),
    };
  });
}

function operatorAllowedOperations(value, field) {
  if (value === undefined || value === null) return null;
  if (!Array.isArray(value) || value.length === 0) {
    throw new Error(`${field} must be a non-empty array`);
  }
  const names = value.map((name, index) => safeContractToken(name, `${field}[${index}]`));
  if (new Set(names).size !== names.length) throw new Error(`${field} repeats an operation name`);
  return names;
}

function operatorArtifactNames(value, field) {
  if (value === undefined || value === null) return [];
  if (!Array.isArray(value)) throw new Error(`${field} must be an array`);
  const names = value.map((name, index) => safeContractToken(name, `${field}[${index}]`));
  if (new Set(names).size !== names.length) throw new Error(`${field} repeats an artifact name`);
  return names;
}

function operatorExternalRelays(value) {
  if (value === undefined || value === null) return [];
  if (!Array.isArray(value)) throw new Error("externalRelays must be an array");
  return value.map((relay, index) => {
    if (!isObject(relay)) throw new Error(`externalRelays[${index}] must be a JSON object`);
    const checkpointId = safeSegment(relay.checkpointId, `externalRelays[${index}].checkpointId`);
    const interfaceId = safeSegment(relay.interfaceId, `externalRelays[${index}].interfaceId`);
    const requiredVerifiedArtifacts = operatorRequiredVerifiedArtifacts(
      relay.requiredVerifiedArtifacts,
      `externalRelays[${index}].requiredVerifiedArtifacts`,
    );
    if (!Array.isArray(relay.outboundInterfaceIds) || relay.outboundInterfaceIds.length === 0) {
      throw new Error(`externalRelays[${index}].outboundInterfaceIds must be a non-empty array`);
    }
    const outboundInterfaceIds = relay.outboundInterfaceIds.map((target, targetIndex) => (
      safeSegment(target, `externalRelays[${index}].outboundInterfaceIds[${targetIndex}]`)
    ));
    if (new Set(outboundInterfaceIds).size !== outboundInterfaceIds.length) {
      throw new Error(`externalRelays[${index}] repeats an outbound interface`);
    }
    return {
      checkpoint_id: checkpointId,
      interface_id: interfaceId,
      outbound_interface_ids: outboundInterfaceIds,
      ...(requiredVerifiedArtifacts.length > 0
        ? { required_verified_artifacts: requiredVerifiedArtifacts }
        : {}),
    };
  });
}

function operatorRequiredVerifiedArtifacts(value, field) {
  if (value === undefined || value === null) return [];
  if (!Array.isArray(value)) throw new Error(`${field} must be an array`);
  const labels = new Set();
  return value.map((requirement, index) => {
    if (!isObject(requirement) || !isObject(requirement.params)) {
      throw new Error(`${field}[${index}] must carry object params`);
    }
    const artifactLabel = safeContractToken(
      requirement.artifactLabel,
      `${field}[${index}].artifactLabel`,
    );
    if (labels.has(artifactLabel)) {
      throw new Error(`${field} repeats artifact label '${artifactLabel}'`);
    }
    labels.add(artifactLabel);
    return {
      mark: safeContractToken(requirement.mark, `${field}[${index}].mark`),
      artifact_label: artifactLabel,
      verifier: safeSegment(requirement.verifier, `${field}[${index}].verifier`),
      params: requirement.params,
    };
  });
}

function operatorModelOutputs(value) {
  if (value === undefined || value === null) return [];
  if (!Array.isArray(value)) throw new Error("modelOutputs must be an array");
  const checkpoints = new Set();
  const interfaces = new Set();
  return value.map((output, index) => {
    if (!isObject(output)) throw new Error(`modelOutputs[${index}] must be a JSON object`);
    const checkpointId = safeSegment(output.checkpointId, `modelOutputs[${index}].checkpointId`);
    const interfaceId = safeSegment(output.interfaceId, `modelOutputs[${index}].interfaceId`);
    if (checkpoints.has(checkpointId)) throw new Error(`modelOutputs repeats checkpoint '${checkpointId}'`);
    if (interfaces.has(interfaceId)) throw new Error(`modelOutputs repeats interface '${interfaceId}'`);
    checkpoints.add(checkpointId);
    interfaces.add(interfaceId);
    const requirement = output.requiredVerifiedArtifact;
    if (!isObject(requirement) || !isObject(requirement.params)) {
      throw new Error(`modelOutputs[${index}].requiredVerifiedArtifact must carry object params`);
    }
    return {
      checkpoint_id: checkpointId,
      interface_id: interfaceId,
      required_verified_artifact: {
        mark: safeContractToken(requirement.mark, `modelOutputs[${index}].requiredVerifiedArtifact.mark`),
        artifact_label: safeContractToken(
          requirement.artifactLabel,
          `modelOutputs[${index}].requiredVerifiedArtifact.artifactLabel`,
        ),
        verifier: safeSegment(
          requirement.verifier,
          `modelOutputs[${index}].requiredVerifiedArtifact.verifier`,
        ),
        params: requirement.params,
      },
    };
  });
}

function operatorPublications(value) {
  if (value === undefined) return [];
  if (!Array.isArray(value)) throw new Error("publications must be an array");
  const seen = new Set();
  return value.map((pin, index) => {
    const field = `publications[${index}]`;
    if (!isObject(pin) || Object.keys(pin).length !== 3
      || !Object.hasOwn(pin, "source") || !Object.hasOwn(pin, "entry_id") || !Object.hasOwn(pin, "revision")) {
      throw new Error(`${field} requires exactly source, entry_id and revision`);
    }
    const [source] = operatorPayloadArtifactBindings([pin.source]);
    source.id = plannedArtifactOwner(source.id);
    const entry_id = plannedArtifactOwner(pin.entry_id);
    if (/[\u0000-\u001f\u007f-\u009f]/.test(source.id + entry_id)) {
      throw new Error(`${field} contains control characters in its identity`);
    }
    if (typeof pin.revision !== "string" || !/^[0-9a-f]{64}$/.test(pin.revision)) {
      throw new Error(`${field}.revision must be an exact content hash`);
    }
    const key = JSON.stringify([source.id, entry_id]);
    if (seen.has(key)) throw new Error(`${field} repeats a selected publication`);
    seen.add(key);
    return { source, entry_id, revision: pin.revision };
  });
}

function assertPublicationExpansion(plan, requested, publications) {
  const profile = requiredObject(plan.authoring_profile, "plan.authoring_profile");
  const expansion = plan.publication_expansion;
  if (expansion === undefined || expansion === null) {
    if (publications.length || canonicalJson(profile) !== canonicalJson(requested)) {
      throw new Error("topology plan authoring profile drifted from the requested topology");
    }
    return;
  }
  const receipt = requiredObject(expansion, "plan.publication_expansion");
  const fields = ["requested", "publications", "defaults", "selected", "expanded"];
  if (Object.keys(receipt).some(field => !fields.includes(field) && field !== "reused_defaults")
    || fields.some(field => !Object.hasOwn(receipt, field))
    || canonicalJson(receipt.requested) !== canonicalJson(requested)
    || canonicalJson(operatorPublications(receipt.publications)) !== canonicalJson(publications)
    || canonicalJson(receipt.expanded) !== canonicalJson(profile)) {
    throw new Error("publication expansion does not preserve its exact authoring request and result");
  }
  const selected = requiredArray(receipt.selected, "publication expansion selected");
  const defaults = requiredArray(receipt.defaults, "publication expansion defaults");
  const reused = operatorPublications(receipt.reused_defaults);
  const expected = new Map(publications.map(pin => [canonicalJson(pin), pin]));
  const owners = new Set();
  for (const declaration of defaults) {
    const owner = safeSegment(declaration?.interface, "publication default interface");
    if (!isObject(declaration) || Object.keys(declaration).length !== 3
      || typeof declaration.descriptor_hash !== "string" || !/^[0-9a-f]{64}$/.test(declaration.descriptor_hash)
      || owners.has(owner)) throw new Error("publication default declaration is malformed or repeated");
    owners.add(owner);
    for (const pin of operatorPublications(declaration.publications)) expected.set(canonicalJson(pin), pin);
  }
  if (selected.length + reused.length === 0) throw new Error("publication expansion has no selected evidence");
  for (const pin of reused) {
    if (publications.some(explicit => canonicalJson(explicit) === canonicalJson(pin))
      || !expected.delete(canonicalJson(pin))
      || !Array.isArray(requested.publication_evidence)
      || !requested.publication_evidence.some(evidence => canonicalJson(evidence.pin) === canonicalJson(pin))) {
      throw new Error("publication expansion reuses a default without its accepted request evidence");
    }
  }
  for (const evidence of selected) {
    const [pin] = operatorPublications([evidence?.pin]);
    if (!expected.delete(canonicalJson(pin)) || !isObject(evidence.entry) || evidence.entry.id !== pin.entry_id) {
      throw new Error("publication expansion evidence differs from its requested and declared selections");
    }
  }
  if (expected.size) throw new Error("publication expansion omits selected evidence");
}

function operatorPayloadArtifactBindings(value) {
  if (value === undefined) return [];
  if (!Array.isArray(value)) throw new Error("payloadArtifactBindings must be an array");
  const ids = new Set();
  return value.map((reference, index) => {
    const field = `payloadArtifactBindings[${index}]`;
    if (!isObject(reference) || Object.keys(reference).length !== 2
      || !Object.hasOwn(reference, "id") || !Object.hasOwn(reference, "content")) {
      throw new Error(`${field} must carry exactly id and content`);
    }
    const id = reference.id;
    if (typeof id !== "string" || !id || id.trim() !== id || ids.has(id)) {
      throw new Error(`${field} has an invalid or repeated exact artifact identity`);
    }
    if (!Array.isArray(reference.content) || reference.content.length !== 32
      || reference.content.some((byte) => !Number.isInteger(byte) || byte < 0 || byte > 255)) {
      throw new Error(`${field}.content must be the actual 32-byte ArtifactRef hash`);
    }
    ids.add(id);
    return { id, content: [...reference.content] };
  });
}

function operatorContextArtifactBindings(value) {
  if (value === undefined || value === null) return [];
  if (!Array.isArray(value)) throw new Error("contextArtifactBindings must be an array");
  const ids = new Set();
  return value.map((binding, index) => {
    if (!isObject(binding)) throw new Error(`contextArtifactBindings[${index}] must be a JSON object`);
    const id = safeSegment(binding.id, `contextArtifactBindings[${index}].id`);
    if (ids.has(id)) throw new Error(`contextArtifactBindings repeats '${id}'`);
    ids.add(id);
    return { id };
  });
}

function operatorSelectedWorkerEnvironmentKinds(value) {
  if (value === undefined || value === null) return [];
  if (!Array.isArray(value)) throw new Error("selectedWorkerEnvironmentKinds must be an array");
  const kinds = new Set();
  return value.map((kind, index) => {
    const name = safeSegment(kind, `selectedWorkerEnvironmentKinds[${index}]`);
    if (kinds.has(name)) throw new Error(`selectedWorkerEnvironmentKinds repeats '${name}'`);
    kinds.add(name);
    return name;
  });
}

function operatorSelectedTools(value) {
  if (value === undefined || value === null) return [];
  if (!Array.isArray(value)) throw new Error("selectedTools must be an array");
  const tools = new Set();
  return value.map((selection, index) => {
    const selected = requiredObject(selection, `selectedTools[${index}]`);
    const unknown = Object.keys(selected).filter((field) => ![
      "tool", "interface", "workerEnvironmentKind",
    ].includes(field));
    if (unknown.length > 0) {
      throw new Error(`selectedTools[${index}] contains unknown fields: ${unknown.join(", ")}`);
    }
    const id = safeContractToken(selected.tool, `selectedTools[${index}].tool`);
    if (tools.has(id)) throw new Error(`selectedTools repeats '${id}'`);
    tools.add(id);
    return {
      tool: id,
      interface: safeSegment(selected.interface, `selectedTools[${index}].interface`),
      worker_environment_kind: safeSegment(
        selected.workerEnvironmentKind,
        `selectedTools[${index}].workerEnvironmentKind`,
      ),
    };
  });
}

function safeContractToken(value, field) {
  const token = requiredString(value, field);
  if (token.length > 160 || !/^[A-Za-z0-9][A-Za-z0-9._-]*$/.test(token)) {
    throw new Error(`${field} must be a compact contract token`);
  }
  return token;
}

function interfaceEndpoint(octopusHome, interfaceId) {
  return `unix://${interfaceSocketPath(octopusHome, interfaceId)}`;
}

function interfaceSocketPath(octopusHome, interfaceId) {
  return path.join(
    octopusHome,
    "app-server-control",
    `${crypto.createHash("sha256").update(safeSegment(interfaceId, "interfaceId")).digest("hex").slice(0, 24)}.sock`,
  );
}

async function interfaceSocketIdentity(octopusHome, interfaceId) {
  let stat;
  try { stat = await fs.lstat(interfaceSocketPath(octopusHome, interfaceId), { bigint: true }); }
  catch (error) {
    if (error?.code === "ENOENT") return null;
    throw error;
  }
  if (!stat.isSocket()) throw new Error("Chat interface endpoint is not a native socket");
  return `${stat.dev}:${stat.ino}:${stat.birthtimeNs}`;
}
