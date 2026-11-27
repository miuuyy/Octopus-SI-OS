module.exports = { parseOctopusOsAuthoringProfile, parseOctopusOsContextArtifactBindings, parseOctopusOsExternalInputs, parseOctopusOsExternalRelays, parseOctopusOsModelOutputs, parseOctopusOsPresentation, parseOctopusOsRequiredVerifiedArtifacts, parseOctopusOsSelectedTools, parseOctopusOsSelectedWorkerEnvironmentKinds, parseOctopusOsTaskAuthoringProfiles, projectOctopusOsRequiredVerifiedArtifacts };

const { payloadArtifactBindings } = require("../contentAddressedArtifact.cjs");
const path = require("node:path");
const { arrayValue, exactObjectKeys, isPlainRecord, objectOrEmpty, requiredContractToken, requiredSafeRegistryId, stringValue } = require("./values.cjs");

function parseOctopusOsPresentation(value, file) {
  exactObjectKeys(
    value,
    ["name", "projects"],
    ["projects"],
    "Octopus OS presentation",
  );
  if (!Array.isArray(value.projects)) {
    throw new Error(`Octopus OS presentation projects must be an array: ${file}`);
  }
  const name = stringValue(value.name);
  if (value.name !== undefined && (!name || [...name].length > 150 || name !== value.name)) {
    throw new Error(`Octopus OS presentation name is invalid: ${file}`);
  }
  const seenEeIds = new Set();
  const projects = value.projects.map((project, index) => {
    exactObjectKeys(
      project,
      ["ee_id", "name", "icon", "color"],
      ["ee_id", "name"],
      `Octopus OS presentation project ${index}`,
    );
    const eeId = requiredSafeRegistryId(
      project.ee_id,
      `Octopus OS presentation project ${index} EE`,
    );
    const name = stringValue(project.name);
    const icon = stringValue(project.icon);
    const color = stringValue(project.color);
    if (
      seenEeIds.has(eeId)
      || !name
      || name.length > 180
      || icon.length > 64
      || color.length > 64
    ) {
      throw new Error(`Octopus OS presentation project ${index} is invalid: ${file}`);
    }
    seenEeIds.add(eeId);
    return { ee_id: eeId, name, icon, color };
  });
  return { name, projects };
}

function parseOctopusOsAuthoringProfile(value, workerChoice, file) {
  exactObjectKeys(
    value,
    [
      "ee_id", "worker_choice_interface", "door_host_slots", "external_inputs", "external_relays",
      "model_outputs", "context_artifact_bindings", "payload_artifact_bindings", "selected_worker_environment_kinds",
      "selected_tools", "publication_evidence",
    ],
    ["ee_id", "worker_choice_interface", "door_host_slots"],
    "Octopus OS authoring profile",
  );
  const eeId = requiredSafeRegistryId(value.ee_id, "Octopus OS authoring profile EE");
  const worker = requiredSafeRegistryId(
    value.worker_choice_interface,
    "Octopus OS authoring profile worker",
  );
  if (workerChoice && worker !== workerChoice) {
    throw new Error(`Octopus OS authoring profile worker conflicts with client choice: ${file}`);
  }
  if (!isPlainRecord(value.door_host_slots)) {
    throw new Error(`Octopus OS authoring profile door_host_slots is not an object: ${file}`);
  }
  const doorHostSlots = {};
  for (const [interfaceId, slots] of Object.entries(value.door_host_slots)) {
    const door = requiredSafeRegistryId(interfaceId, "Octopus OS authoring profile door");
    if (!isPlainRecord(slots) || Object.keys(slots).length === 0) {
      throw new Error(`Octopus OS authoring profile door '${door}' has no host slots: ${file}`);
    }
    const normalized = {};
    for (const [slotName, root] of Object.entries(slots)) {
      const slot = requiredSafeRegistryId(slotName, "Octopus OS authoring profile root slot");
      const rootPath = stringValue(root);
      if (!path.isAbsolute(rootPath)) {
        throw new Error(`Octopus OS authoring profile root '${door}.${slot}' is not absolute: ${file}`);
      }
      normalized[slot] = rootPath;
    }
    doorHostSlots[door] = normalized;
  }
  const externalInputs = parseOctopusOsExternalInputs(value.external_inputs, file);
  const externalRelays = parseOctopusOsExternalRelays(value.external_relays, file);
  const modelOutputs = parseOctopusOsModelOutputs(value.model_outputs, file);
  const contextArtifactBindings = parseOctopusOsContextArtifactBindings(
    value.context_artifact_bindings,
    file,
  );
  const selectedPayloads = payloadArtifactBindings(value.payload_artifact_bindings);
  if (selectedPayloads.some(reference => contextArtifactBindings.some(context => context.id === reference.id))) throw new Error("Payload and JSON context selections overlap");
  const selectedWorkerEnvironmentKinds = parseOctopusOsSelectedWorkerEnvironmentKinds(
    value.selected_worker_environment_kinds,
    file,
  );
  const selectedTools = parseOctopusOsSelectedTools(value.selected_tools, file);
  const publicationEvidence = parsePublicationSelectionEvidence(value.publication_evidence);
  const routeCheckpoints = new Set(externalInputs.map((input) => input.checkpoint_id));
  const routeSources = new Set(externalInputs.map((input) => input.interface_id));
  for (const relay of externalRelays) {
    if (routeCheckpoints.has(relay.checkpoint_id) || routeSources.has(relay.interface_id)) {
      throw new Error(`Octopus OS authoring profile repeats an external route: ${file}`);
    }
    routeCheckpoints.add(relay.checkpoint_id);
    routeSources.add(relay.interface_id);
  }
  for (const output of modelOutputs) {
    if (routeCheckpoints.has(output.checkpoint_id)) {
      throw new Error(`Octopus OS authoring profile repeats an external checkpoint: ${file}`);
    }
    routeCheckpoints.add(output.checkpoint_id);
  }
  return {
    ee_id: eeId,
    worker_choice_interface: worker,
    door_host_slots: doorHostSlots,
    external_inputs: externalInputs,
    external_relays: externalRelays,
    model_outputs: modelOutputs,
    context_artifact_bindings: contextArtifactBindings,
    ...(selectedPayloads.length ? {payload_artifact_bindings:selectedPayloads} : {}),
    selected_worker_environment_kinds: selectedWorkerEnvironmentKinds,
    selected_tools: selectedTools,
    ...(publicationEvidence.length ? { publication_evidence: publicationEvidence } : {}),
  };
}

function parsePublicationSelectionEvidence(value) {
  if (value === undefined) return [];
  if (!Array.isArray(value)) throw new Error("Publication selection evidence must be an array");
  const seen = new Set();
  return value.map((evidence) => {
    exactObjectKeys(evidence, ["pin", "entry", "snapshot", "context_sources"], ["pin", "entry"], "Publication evidence");
    exactObjectKeys(evidence.pin, ["source", "entry_id", "revision"], ["source", "entry_id", "revision"], "Publication pin");
    const source = payloadArtifactBindings([evidence.pin.source])[0];
    const entryId = requiredContractToken(evidence.pin.entry_id, "Publication entry id");
    const key = `${source.id}\u0000${entryId}`;
    if (!/^[0-9a-f]{64}$/.test(evidence.pin.revision) || !isPlainRecord(evidence.entry)
      || evidence.entry.id !== entryId || seen.has(key)) throw new Error("Invalid publication selection evidence");
    seen.add(key);
    if (evidence.snapshot !== undefined) payloadArtifactBindings([evidence.snapshot]);
    payloadArtifactBindings(evidence.context_sources);
    return evidence;
  });
}

function parseOctopusOsTaskAuthoringProfiles(value, file) {
  if (!Array.isArray(value)) {
    throw new Error(`Octopus OS task authoring profiles must be an array: ${file}`);
  }
  const seen = new Set();
  return value.map((candidate, index) => {
    const profile = parseOctopusOsAuthoringProfile(candidate, "", file);
    const key = `${profile.ee_id}\u0000${profile.worker_choice_interface}`;
    if (seen.has(key)) {
      throw new Error(`Octopus OS task authoring profile ${index} repeats EE and worker: ${file}`);
    }
    seen.add(key);
    return profile;
  });
}

function parseOctopusOsExternalInputs(value, file) {
  if (value === undefined) return [];
  if (!Array.isArray(value)) throw new Error(`Octopus OS external_inputs must be an array: ${file}`);
  const checkpoints = new Set();
  const interfaces = new Set();
  return value.map((candidate, index) => {
    exactObjectKeys(
      candidate,
      ["checkpoint_id", "interface_id", "artifact_names", "required_verified_artifacts"],
      ["checkpoint_id", "interface_id"],
      `Octopus OS external input ${index}`,
    );
    const checkpointId = requiredSafeRegistryId(candidate.checkpoint_id, `Octopus OS external input ${index} checkpoint`);
    const interfaceId = requiredSafeRegistryId(candidate.interface_id, `Octopus OS external input ${index} interface`);
    const artifactNames = parseOctopusOsArtifactNames(candidate.artifact_names, `Octopus OS external input ${index}`, file);
    if (checkpoints.has(checkpointId) || interfaces.has(interfaceId)) {
      throw new Error(`Octopus OS external input ${index} is duplicated: ${file}`);
    }
    checkpoints.add(checkpointId);
    interfaces.add(interfaceId);
    const requirements = parseOctopusOsRequiredVerifiedArtifacts(
      candidate.required_verified_artifacts,
      `Octopus OS external input ${index}`,
      file,
    );
    return {
      checkpoint_id: checkpointId,
      interface_id: interfaceId,
      ...(artifactNames.length > 0 ? { artifact_names: artifactNames } : {}),
      ...(requirements.length > 0 ? { required_verified_artifacts: requirements } : {}),
    };
  });
}

function parseOctopusOsArtifactNames(value, owner, file) {
  if (value === undefined) return [];
  if (!Array.isArray(value) || value.length === 0) {
    throw new Error(`${owner} artifact_names must be a non-empty array: ${file}`);
  }
  const names = value.map((name, index) => requiredContractToken(name, `${owner} artifact name ${index}`));
  if (new Set(names).size !== names.length) throw new Error(`${owner} repeats an artifact name: ${file}`);
  return names;
}

function parseOctopusOsExternalRelays(value, file) {
  if (value === undefined) return [];
  if (!Array.isArray(value)) throw new Error(`Octopus OS external_relays must be an array: ${file}`);
  const checkpoints = new Set();
  const interfaces = new Set();
  return value.map((candidate, index) => {
    exactObjectKeys(
      candidate,
      ["checkpoint_id", "interface_id", "outbound_interface_ids", "required_verified_artifacts"],
      ["checkpoint_id", "interface_id", "outbound_interface_ids"],
      `Octopus OS external relay ${index}`,
    );
    const checkpointId = requiredSafeRegistryId(candidate.checkpoint_id, `Octopus OS external relay ${index} checkpoint`);
    const interfaceId = requiredSafeRegistryId(candidate.interface_id, `Octopus OS external relay ${index} interface`);
    if (!Array.isArray(candidate.outbound_interface_ids) || candidate.outbound_interface_ids.length === 0) {
      throw new Error(`Octopus OS external relay ${index} has no outbound interfaces: ${file}`);
    }
    const outboundInterfaceIds = candidate.outbound_interface_ids.map((target, targetIndex) => (
      requiredSafeRegistryId(target, `Octopus OS external relay ${index} outbound interface ${targetIndex}`)
    ));
    if (
      checkpoints.has(checkpointId)
      || interfaces.has(interfaceId)
      || new Set(outboundInterfaceIds).size !== outboundInterfaceIds.length
    ) {
      throw new Error(`Octopus OS external relay ${index} is duplicated: ${file}`);
    }
    checkpoints.add(checkpointId);
    interfaces.add(interfaceId);
    const requirements = parseOctopusOsRequiredVerifiedArtifacts(
      candidate.required_verified_artifacts,
      `Octopus OS external relay ${index}`,
      file,
    );
    return {
      checkpoint_id: checkpointId,
      interface_id: interfaceId,
      outbound_interface_ids: outboundInterfaceIds,
      ...(requirements.length > 0 ? { required_verified_artifacts: requirements } : {}),
    };
  });
}

function parseOctopusOsRequiredVerifiedArtifacts(value, owner, file) {
  if (value === undefined) return [];
  if (!Array.isArray(value)) {
    throw new Error(`${owner} required_verified_artifacts must be an array: ${file}`);
  }
  const labels = new Set();
  return value.map((requirement, index) => {
    exactObjectKeys(
      requirement,
      ["mark", "artifact_label", "verifier", "params"],
      ["mark", "artifact_label", "verifier", "params"],
      `${owner} requirement ${index}`,
    );
    if (!isPlainRecord(requirement.params)) {
      throw new Error(`${owner} requirement ${index} params are not an object: ${file}`);
    }
    const artifactLabel = requiredContractToken(
      requirement.artifact_label,
      `${owner} requirement ${index} artifact label`,
    );
    if (labels.has(artifactLabel)) {
      throw new Error(`${owner} repeats required artifact label '${artifactLabel}': ${file}`);
    }
    labels.add(artifactLabel);
    return {
      mark: requiredContractToken(requirement.mark, `${owner} requirement ${index} mark`),
      artifact_label: artifactLabel,
      verifier: requiredSafeRegistryId(
        requirement.verifier,
        `${owner} requirement ${index} verifier`,
      ),
      params: requirement.params,
    };
  });
}

function projectOctopusOsRequiredVerifiedArtifacts(value) {
  return arrayValue(value).map((requirement) => ({
    mark: stringValue(requirement?.mark),
    artifactLabel: stringValue(requirement?.artifact_label),
    verifier: stringValue(requirement?.verifier),
    params: objectOrEmpty(requirement?.params),
  }));
}

function parseOctopusOsModelOutputs(value, file) {
  if (value === undefined) return [];
  if (!Array.isArray(value)) throw new Error(`Octopus OS model_outputs must be an array: ${file}`);
  const checkpoints = new Set();
  const interfaces = new Set();
  return value.map((candidate, index) => {
    exactObjectKeys(
      candidate,
      ["checkpoint_id", "interface_id", "required_verified_artifact"],
      ["checkpoint_id", "interface_id", "required_verified_artifact"],
      `Octopus OS model output ${index}`,
    );
    const checkpointId = requiredSafeRegistryId(candidate.checkpoint_id, `Octopus OS model output ${index} checkpoint`);
    const interfaceId = requiredSafeRegistryId(candidate.interface_id, `Octopus OS model output ${index} interface`);
    if (checkpoints.has(checkpointId) || interfaces.has(interfaceId)) {
      throw new Error(`Octopus OS model output ${index} is duplicated: ${file}`);
    }
    checkpoints.add(checkpointId);
    interfaces.add(interfaceId);
    const requirement = candidate.required_verified_artifact;
    exactObjectKeys(
      requirement,
      ["mark", "artifact_label", "verifier", "params"],
      ["mark", "artifact_label", "verifier", "params"],
      `Octopus OS model output ${index} requirement`,
    );
    if (!isPlainRecord(requirement.params)) {
      throw new Error(`Octopus OS model output ${index} requirement params are not an object: ${file}`);
    }
    return {
      checkpoint_id: checkpointId,
      interface_id: interfaceId,
      required_verified_artifact: {
        mark: requiredContractToken(requirement.mark, `Octopus OS model output ${index} mark`),
        artifact_label: requiredContractToken(
          requirement.artifact_label,
          `Octopus OS model output ${index} artifact label`,
        ),
        verifier: requiredSafeRegistryId(requirement.verifier, `Octopus OS model output ${index} verifier`),
        params: requirement.params,
      },
    };
  });
}

function parseOctopusOsContextArtifactBindings(value, file) {
  if (value === undefined) return [];
  if (!Array.isArray(value)) throw new Error(`Octopus OS context_artifact_bindings must be an array: ${file}`);
  const ids = new Set();
  return value.map((candidate, index) => {
    exactObjectKeys(candidate, ["id"], ["id"], `Octopus OS context artifact binding ${index}`);
    const id = requiredSafeRegistryId(candidate.id, `Octopus OS context artifact binding ${index}`);
    if (ids.has(id)) throw new Error(`Octopus OS context artifact binding '${id}' is duplicated: ${file}`);
    ids.add(id);
    return { id };
  });
}

function parseOctopusOsSelectedWorkerEnvironmentKinds(value, file) {
  if (value === undefined) return [];
  if (!Array.isArray(value)) throw new Error(`Octopus OS selected_worker_environment_kinds must be an array: ${file}`);
  const kinds = value.map((candidate, index) => (
    requiredSafeRegistryId(candidate, `Octopus OS selected worker environment kind ${index}`)
  ));
  if (new Set(kinds).size !== kinds.length) {
    throw new Error(`Octopus OS selected_worker_environment_kinds contains duplicates: ${file}`);
  }
  return kinds;
}

function parseOctopusOsSelectedTools(value, file) {
  if (value === undefined) return [];
  if (!Array.isArray(value)) throw new Error(`Octopus OS selected_tools must be an array: ${file}`);
  const tools = value.map((candidate, index) => {
    exactObjectKeys(
      candidate,
      ["tool", "interface", "worker_environment_kind"],
      ["tool", "interface", "worker_environment_kind"],
      `Octopus OS selected tool ${index}`,
    );
    return {
      tool: requiredContractToken(candidate.tool, `Octopus OS selected tool ${index}`),
      interface: requiredSafeRegistryId(candidate.interface, `Octopus OS selected tool ${index} interface`),
      worker_environment_kind: requiredSafeRegistryId(
        candidate.worker_environment_kind,
        `Octopus OS selected tool ${index} worker environment kind`,
      ),
    };
  });
  if (new Set(tools.map((selection) => selection.tool)).size !== tools.length) {
    throw new Error(`Octopus OS selected_tools contains duplicates: ${file}`);
  }
  return tools;
}
