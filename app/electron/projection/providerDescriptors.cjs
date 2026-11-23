module.exports = { listDirectFiles, listWorkerEnvironmentRecordFiles, normalizeLabelList, providerInterfaceDescriptorRows, providerLabel, providerModelCatalogRows, readDirectoryEntries, readOperatorChatProtocol, readProviderInterfaceDescriptors };

const { modelServiceTiers } = require("../providerModelTiers.cjs");
const fs = require("node:fs/promises");
const path = require("node:path");
const { readJsonFile } = require("./jsonFiles.cjs");
const { parseOctopusOsSidecar } = require("./sidecars.cjs");
const { arrayValue, objectOrEmpty, stringValue } = require("./values.cjs");

async function readProviderInterfaceDescriptors(registryRoot) {
  const interfaceRoot = path.join(registryRoot, "interface");
  const entries = await fs.readdir(interfaceRoot, { withFileTypes: true });
  const descriptors = [];
  for (const entry of entries) {
    if (!entry.isDirectory()) continue;
    const descriptorPath = path.join(interfaceRoot, entry.name, "interface.json");
    let descriptor;
    try {
      descriptor = JSON.parse(await fs.readFile(descriptorPath, "utf8"));
    } catch {
      continue;
    }
    if (stringValue(descriptor.interface_type).toLowerCase() !== "world") continue;
    if (stringValue(descriptor.role).toLowerCase() !== "provider") continue;
    const id = stringValue(descriptor.id) || entry.name;
    if (id !== entry.name) continue;
    const octopusOsPath = path.join(interfaceRoot, entry.name, "octopus_os.json");
    const octopusOs = await readJsonFile(octopusOsPath);
    if (octopusOs) {
      parseOctopusOsSidecar(octopusOs, id, octopusOsPath);
      continue;
    }
    descriptors.push({
      ...descriptor,
      descriptor_path: descriptorPath,
      registry_dir: entry.name,
    });
  }
  return providerInterfaceDescriptorRows(descriptors);
}

function providerInterfaceDescriptorRows(descriptors) {
  const providers = [];
  for (const descriptor of descriptors) {
    if (stringValue(descriptor.interface_type).toLowerCase() !== "world") continue;
    if (stringValue(descriptor.role).toLowerCase() !== "provider") continue;
    if (descriptor.octopus_os_sidecar) continue;
    const id = stringValue(descriptor.id) || stringValue(descriptor.registry_dir);
    if (!id || id !== stringValue(descriptor.registry_dir)) continue;
    const descriptorConfig = objectOrEmpty(descriptor.interface_driver_config);
    const privateConfig = objectOrEmpty(descriptorConfig.private_config);
    const supportedReasoningEfforts = arrayValue(
      descriptorConfig.supported_reasoning_efforts,
    ).map((value) => stringValue(value?.effort ?? value)).filter(Boolean);
    const model = stringValue(privateConfig.model);
    if (!model) continue;
    const declaredModels = arrayValue(
      objectOrEmpty(objectOrEmpty(objectOrEmpty(descriptor.interface_policy).variables).models).default,
    ).map(stringValue).filter(Boolean);
    const models = providerModelCatalogRows(descriptorConfig, privateConfig, model, declaredModels);
    providers.push({
      id,
      label: providerLabel(descriptor, id),
      descriptorPath: stringValue(descriptor.descriptor_path),
      runtimeHandlerRef: stringValue(descriptor.interface_driver_ref),
      sourceNames: arrayValue(descriptor.source_names).map(stringValue).filter(Boolean),
      accountAuthKind: privateConfig.client_metadata !== undefined
        ? "client_metadata"
        : privateConfig.client_connection !== undefined
          ? "client_connection"
          : "credential",
      model,
      reasoningEffort: stringValue(privateConfig.reasoning_effort),
      serviceTier: stringValue(privateConfig.service_tier),
      supportedReasoningEfforts,
      models,
    });
  }
  return providers.sort((a, b) => a.id.localeCompare(b.id));
}

function providerModelCatalogRows(descriptorConfig, privateConfig, selectedModel, declaredModels = []) {
  const globalEfforts = arrayValue(descriptorConfig.supported_reasoning_efforts)
    .map((value) => stringValue(value?.effort ?? value))
    .filter(Boolean);
  const selectedEffort = stringValue(privateConfig.reasoning_effort);
  const rows = [];
  const seen = new Set();
  for (const value of arrayValue(privateConfig.model_catalog)) {
    const entry = objectOrEmpty(value);
    const model = stringValue(entry.model);
    if (!model || seen.has(model)) continue;
    seen.add(model);
    const supportedReasoningEfforts = arrayValue(
      entry.supported_reasoning_efforts ?? entry.supportedReasoningEfforts,
    ).map((effort) => stringValue(effort?.effort ?? effort?.reasoningEffort ?? effort))
      .filter(Boolean);
    rows.push({
      model,
      displayName: stringValue(entry.display_name ?? entry.displayName) || model,
      description: stringValue(entry.description),
      defaultReasoningEffort: stringValue(
        entry.default_reasoning_effort ?? entry.defaultReasoningEffort,
      ) || (model === selectedModel ? selectedEffort : ""),
      supportedReasoningEfforts: supportedReasoningEfforts.length
        ? [...new Set(supportedReasoningEfforts)]
        : model === selectedModel ? [...new Set(globalEfforts)] : [],
      showInPicker: entry.show_in_picker !== false && entry.showInPicker !== false,
      isDefault: entry.is_default === true || entry.isDefault === true || model === selectedModel,
      ...modelServiceTiers(entry.service_tiers, entry.default_service_tier, `Provider model '${model}'`),
    });
  }
  for (const model of declaredModels) {
    if (!model || seen.has(model)) continue;
    seen.add(model);
    rows.push({
      model,
      displayName: model,
      description: "",
      defaultReasoningEffort: model === selectedModel ? selectedEffort : "",
      supportedReasoningEfforts: model === selectedModel ? [...new Set(globalEfforts)] : [],
      showInPicker: true,
      isDefault: model === selectedModel,
    });
  }
  if (!seen.has(selectedModel)) {
    rows.push({
      model: selectedModel,
      displayName: selectedModel,
      description: "",
      defaultReasoningEffort: selectedEffort,
      supportedReasoningEfforts: [...new Set(globalEfforts)],
      showInPicker: true,
      isDefault: true,
    });
  }
  return rows;
}

async function readOperatorChatProtocol(registryRoot) {
  try {
    const client = JSON.parse(await fs.readFile(
      path.join(registryRoot, "interface", "world_operator_chat", "client.json"),
      "utf8",
    ));
    const labels = objectOrEmpty(client.artifact_labels);
    const authoring = objectOrEmpty(client.authoring);
    const protocol = {
      interfaceId: stringValue(client.interface),
      outboundInterfaceId: stringValue(client.outbound_interface) || stringValue(client.interface),
      inputLabel: stringValue(labels.message),
      terminalOutputLabel: stringValue(labels.terminal_output),
      errorOutputLabel: stringValue(labels.error_output),
      streamOutputLabels: normalizeLabelList(labels.stream_outputs, []),
      approvalResponseLabel: stringValue(labels.approval_response),
      elicitationResponseLabel: stringValue(labels.elicitation_response),
      userInputResponseLabel: stringValue(labels.user_input_response),
      toolCallLabel: stringValue(labels.tool_call),
      retryOutputLabel: stringValue(labels.retry_output),
      hiddenActivityLabels: normalizeLabelList(labels.hidden_activity, []),
      authoringRoomInterfaceId: stringValue(authoring.room_interface),
      artifactReadProjection: client.artifact_read_projection,
    };
    return Object.values(protocol).slice(0, 5).every(Boolean) ? protocol : null;
  } catch {
    return null;
  }
}

function providerLabel(descriptor, fallback) {
  const source = arrayValue(descriptor.source_names).map(stringValue).find(Boolean);
  return source || fallback;
}

function normalizeLabelList(values, fallback) {
  const normalized = [];
  const seen = new Set();
  for (const value of [...arrayValue(values), ...fallback]) {
    const text = stringValue(value);
    if (!text || seen.has(text)) continue;
    seen.add(text);
    normalized.push(text);
  }
  return normalized;
}

async function readDirectoryEntries(root) {
  try {
    return await fs.readdir(root, { withFileTypes: true });
  } catch (error) {
    if (error?.code === "ENOENT") return [];
    throw error;
  }
}

async function listDirectFiles(root, predicate) {
  const entries = await readDirectoryEntries(root);
  return entries
    .filter((entry) => entry.isFile())
    .map((entry) => path.join(root, entry.name))
    .filter(predicate)
    .sort();
}

async function listWorkerEnvironmentRecordFiles(interfacesRoot) {
  const interfaces = (await readDirectoryEntries(interfacesRoot)).filter((entry) => entry.isDirectory());
  const nested = await Promise.all(interfaces.map(async (interfaceEntry) => {
    const root = path.join(interfacesRoot, interfaceEntry.name, "worker-environments");
    return (await readDirectoryEntries(root))
      .filter((entry) => entry.isDirectory())
      .map((entry) => path.join(root, entry.name, "worker_environment.json"));
  }));
  return nested.flat().sort();
}
