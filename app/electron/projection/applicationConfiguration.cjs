module.exports = { assertApplicationToolCapability, assertCapabilityValuesWithinCeiling, configureOctopusOsApplication, configureOctopusOsDriver, configureOctopusOsProjectDefaults, installedDescriptorIds, parseDriverHostSlots, prepareProjectCapabilityState, projectConfiguredApplication };

const fs = require("node:fs/promises");
const path = require("node:path");
const { projectCapabilityChanges } = require("./applicationPackages.cjs");
const { readOptionalRegularJsonDocument, readRegularJsonDocument } = require("./jsonFiles.cjs");
const { readDirectoryEntries } = require("./providerDescriptors.cjs");
const { parseApplicationManifest } = require("./sidecars.cjs");
const { readInstalledApplicationManifestDocuments } = require("./taskApplications.cjs");
const { scopedRecordPath } = require("./taskDeletion.cjs");
const { arrayValue, isPlainRecord, objectOrEmpty, requiredDigest, requiredSafeRegistryId, safeInterfacePathSegment, stringValue } = require("./values.cjs");

async function configureOctopusOsApplication(
  registryRoot,
  octopusHomeRoot,
  scope,
  request,
  commitClientAuthoring,
) {
  if (typeof commitClientAuthoring !== "function") {
    throw new Error("client authoring transaction is unavailable");
  }
  const applicationId = requiredSafeRegistryId(request?.applicationId, "applicationId");
  const projects = projectCapabilityChanges(request, false);
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
  const allInterfaces = await installedDescriptorIds(
    path.join(registryRoot, "interface"),
    "interface.json",
    ["id", "rule_id"],
  );
  const allTools = await installedDescriptorIds(
    path.join(registryRoot, "tool"),
    "tool.json",
    ["id"],
  );
  const allKinds = await installedDescriptorIds(
    path.join(registryRoot, "worker_environment_kind"),
    "worker_environment_kind.json",
    ["worker_environment_kind_id", "worker_environment_kind", "kind", "id"],
  );
  const applicationDocuments = await readInstalledApplicationManifestDocuments(registryRoot);
  const requiredInterfaces = new Set(manifest.interfaces);
  const requiredTools = new Set();
  const requiredKinds = new Set();
  for (const capability of manifest.task_capabilities.tools) {
    requiredInterfaces.add(capability.interface);
    requiredTools.add(capability.tool);
    requiredKinds.add(capability.worker_environment_kind);
    await assertApplicationToolCapability(registryRoot, applicationId, capability);
  }
  for (const interfaceId of requiredInterfaces) {
    if (!allInterfaces.includes(interfaceId)) {
      throw new Error(`application '${applicationId}' references missing interface '${interfaceId}'`);
    }
  }
  const prepared = [];
  for (const project of projects) {
    const environment = scope.byId.get(project.eeId);
    if (!environment) throw new Error(`project EE '${project.eeId}' is outside the Octopus OS scope`);
    const retainedApplications = applicationDocuments.filter((candidate) => (
      candidate.manifest.id !== applicationId
      && projectConfiguredApplication(environment, candidate.manifest)
    ));
    const retainedInterfaces = new Set(retainedApplications.flatMap((candidate) => [
      ...candidate.manifest.interfaces,
      ...candidate.manifest.task_capabilities.tools.map((capability) => capability.interface),
    ]));
    const retainedTools = new Set(retainedApplications.flatMap((candidate) => (
      candidate.manifest.task_capabilities.tools.map((capability) => capability.tool)
    )));
    const retainedKinds = new Set(retainedApplications.flatMap((candidate) => (
      candidate.manifest.task_capabilities.tools.map((capability) => capability.worker_environment_kind)
    )));
    prepared.push(await prepareProjectCapabilityState({
      octopusHomeRoot,
      environment,
      eeId: project.eeId,
      expectedDigest: project.expectedDigest,
      capabilityId: applicationId,
      available: project.available,
      selectedByDefault: project.selectedByDefault,
      axisUpdates: [
        {
          field: "allowed_interfaces",
          values: [...requiredInterfaces],
          installedValues: allInterfaces,
          removeValues: [...requiredInterfaces].filter((value) => !retainedInterfaces.has(value)),
        },
        {
          field: "allowed_tools",
          values: [...requiredTools],
          installedValues: allTools,
          removeValues: [...requiredTools].filter((value) => !retainedTools.has(value)),
        },
        {
          field: "allowed_worker_environment_kinds",
          values: [...requiredKinds],
          installedValues: allKinds,
          removeValues: [...requiredKinds].filter((value) => !retainedKinds.has(value)),
        },
      ],
      defaultField: "application_defaults",
    }));
  }
  const writes = prepared.map((entry) => entry.write);
  await commitClientAuthoring({
    label: `configure_application ${applicationId}`,
    writes,
    removals: [],
    publishBarrierPath: writes.at(-1)?.file,
  });
  return {
    action: "configure_application",
    capabilityId: applicationId,
    projectChanges: prepared.map((entry) => entry.outcome),
  };
}

async function assertApplicationToolCapability(registryRoot, applicationId, capability) {
  const toolPath = path.join(
    await fs.realpath(path.join(registryRoot, "tool")),
    capability.tool,
    "tool.json",
  );
  const tool = (await readRegularJsonDocument(
    toolPath,
    `application '${applicationId}' tool ${capability.tool}`,
  )).document;
  if (
    stringValue(tool.id) !== capability.tool
    || stringValue(tool.interface) !== capability.interface
    || !arrayValue(tool.supported_worker_environment_kinds)
      .map(stringValue)
      .includes(capability.worker_environment_kind)
  ) {
    throw new Error(
      `application '${applicationId}' task capability '${capability.tool}' does not match its exact tool/door/worker-environment descriptor`,
    );
  }
}

function projectConfiguredApplication(environment, manifest) {
  const effective = objectOrEmpty(environment.formed_envelope);
  const allowed = (field, value) => (
    !Array.isArray(effective[field]) || effective[field].includes(value)
  );
  return manifest.interfaces.every((interfaceId) => allowed("allowed_interfaces", interfaceId))
    && manifest.task_capabilities.tools.every((capability) => (
      allowed("allowed_interfaces", capability.interface)
      && allowed("allowed_tools", capability.tool)
      && allowed("allowed_worker_environment_kinds", capability.worker_environment_kind)
    ));
}

async function configureOctopusOsDriver(
  registryRoot,
  octopusHomeRoot,
  scope,
  request,
  commitClientAuthoring,
) {
  if (typeof commitClientAuthoring !== "function") {
    throw new Error("client authoring transaction is unavailable");
  }
  const driverName = requiredSafeRegistryId(request?.driverName, "driverName");
  const projects = projectCapabilityChanges(request, true);
  const kindRoot = await fs.realpath(path.join(registryRoot, "worker_environment_kind"));
  const descriptorPath = path.join(kindRoot, driverName, "worker_environment_kind.json");
  const descriptor = (await readRegularJsonDocument(
    descriptorPath,
    `worker environment kind ${driverName}`,
  )).document;
  const declaredName = stringValue(
    descriptor.worker_environment_kind_id
    ?? descriptor.worker_environment_kind
    ?? descriptor.kind
    ?? descriptor.id,
  );
  if (declaredName !== driverName || stringValue(descriptor.worker_consent) !== "explicit_ceiling") {
    throw new Error(`driver '${driverName}' does not satisfy the current worker-environment-kind contract`);
  }
  const doorDriverRefs = arrayValue(descriptor.door_driver_refs)
    .map((value) => requiredSafeRegistryId(value, `driver '${driverName}' door driver ref`));
  if (doorDriverRefs.length === 0 || new Set(doorDriverRefs).size !== doorDriverRefs.length) {
    throw new Error(`driver '${driverName}' has no unique door driver refs`);
  }
  const allKinds = await installedDescriptorIds(
    kindRoot,
    "worker_environment_kind.json",
    ["worker_environment_kind_id", "worker_environment_kind", "kind", "id"],
  );
  const toolRoot = await fs.realpath(path.join(registryRoot, "tool"));
  const allTools = await installedDescriptorIds(toolRoot, "tool.json", ["id"]);
  const toolIds = [];
  const doorInterfaces = new Set();
  for (const toolId of allTools) {
    const tool = (await readRegularJsonDocument(
      path.join(toolRoot, toolId, "tool.json"),
      `tool descriptor ${toolId}`,
    )).document;
    if (arrayValue(tool.supported_worker_environment_kinds).map(stringValue).includes(driverName)) {
      toolIds.push(toolId);
      doorInterfaces.add(requiredSafeRegistryId(tool.interface, `tool '${toolId}' owner interface`));
    }
  }
  if (doorInterfaces.size === 0) {
    throw new Error(`driver '${driverName}' has no tool-owned door interface`);
  }
  const allInterfaces = await installedDescriptorIds(
    path.join(registryRoot, "interface"),
    "interface.json",
    ["id", "rule_id"],
  );
  const prepared = [];
  for (const project of projects) {
    const environment = scope.byId.get(project.eeId);
    if (!environment) throw new Error(`project EE '${project.eeId}' is outside the Octopus OS scope`);
    prepared.push(await prepareProjectCapabilityState({
      octopusHomeRoot,
      environment,
      eeId: project.eeId,
      expectedDigest: project.expectedDigest,
      capabilityId: driverName,
      available: project.available,
      selectedByDefault: project.selectedByDefault,
      axisUpdates: [
        {
          field: "allowed_worker_environment_kinds",
          values: [driverName],
          installedValues: allKinds,
          removeValues: [driverName],
        },
        {
          field: "allowed_interfaces",
          values: [...doorInterfaces],
          installedValues: allInterfaces,
        },
        {
          field: "allowed_tools",
          values: toolIds,
          installedValues: allTools,
        },
      ],
      defaultField: "driver_defaults",
      driverHostSlots: parseDriverHostSlots(project.hostSlots, descriptor, driverName),
    }));
  }
  const writes = prepared.map((entry) => entry.write);
  await commitClientAuthoring({
    label: `configure_driver ${driverName}`,
    writes,
    removals: [],
    publishBarrierPath: writes.at(-1)?.file,
  });
  return {
    action: "configure_driver",
    capabilityId: driverName,
    projectChanges: prepared.map((entry) => entry.outcome),
  };
}

async function prepareProjectCapabilityState({
  octopusHomeRoot,
  environment,
  eeId,
  expectedDigest,
  capabilityId,
  available,
  selectedByDefault,
  axisUpdates,
  defaultField,
  driverHostSlots,
}) {
  const digest = requiredDigest(expectedDigest, "expectedDigest");
  const eePath = await scopedRecordPath(
    octopusHomeRoot,
    environment.record_path,
    path.join("state", "ee_instances"),
    `${eeId}.json`,
  );
  const current = await readRegularJsonDocument(eePath, `project EE ${eeId}`);
  if (current.digest !== digest || stringValue(environment.record_digest) !== digest) {
    throw new Error(`project EE '${eeId}' changed before its capability setup could be applied`);
  }
  if (stringValue(current.document.ee_id) !== eeId) {
    throw new Error(`project EE '${eeId}' has drifted identity`);
  }
  const next = JSON.parse(JSON.stringify(current.document));
  applyProjectCapabilityStateMutation({
    eeId,
    environment,
    next,
    capabilityId,
    available,
    selectedByDefault,
    axisUpdates,
    defaultField,
    driverHostSlots,
  });
  return {
    write: {
      file: eePath,
      beforeDigest: digest,
      next,
    },
    outcome: {
      eeId,
      available,
      selectedByDefault,
    },
  };
}

function applyProjectCapabilityStateMutation({
  eeId,
  environment,
  next,
  capabilityId,
  available,
  selectedByDefault,
  axisUpdates,
  defaultField,
  driverHostSlots,
}) {
  const narrowing = isPlainRecord(next.policy_narrowing) ? next.policy_narrowing : {};
  const ceiling = objectOrEmpty(environment.boundary_envelope);
  for (const update of axisUpdates) {
    if (available) assertCapabilityValuesWithinCeiling(ceiling, update.field, update.values, capabilityId);
    const authored = Object.prototype.hasOwnProperty.call(narrowing, update.field)
      ? arrayValue(narrowing[update.field]).map(stringValue).filter(Boolean)
      : null;
    if (authored === null && available) continue;
    if (!available && !update.removeValues?.length) continue;
    const base = authored ?? update.installedValues.filter((value) => (
      !Array.isArray(ceiling[update.field]) || ceiling[update.field].includes(value)
    ));
    const values = new Set(base);
    if (available) {
      for (const value of update.values) values.add(value);
    } else {
      for (const value of update.removeValues) values.delete(value);
    }
    narrowing[update.field] = [...values].sort();
  }
  next.policy_narrowing = narrowing;
  const config = isPlainRecord(next.config) ? next.config : {};
  if (config.octopus_os !== undefined && !isPlainRecord(config.octopus_os)) {
    throw new Error(`project EE '${eeId}' has malformed Octopus OS client config`);
  }
  const octopusOs = isPlainRecord(config.octopus_os) ? config.octopus_os : {};
  const defaults = new Set(arrayValue(octopusOs[defaultField]).map(stringValue).filter(Boolean));
  if (selectedByDefault) defaults.add(capabilityId);
  else defaults.delete(capabilityId);
  octopusOs[defaultField] = [...defaults].sort();
  if (driverHostSlots !== undefined) {
    if (octopusOs.driver_host_slots !== undefined && !isPlainRecord(octopusOs.driver_host_slots)) {
      throw new Error(`project EE '${eeId}' has malformed driver host-slot config`);
    }
    const driverSlots = isPlainRecord(octopusOs.driver_host_slots)
      ? octopusOs.driver_host_slots
      : {};
    if (available) driverSlots[capabilityId] = driverHostSlots;
    else delete driverSlots[capabilityId];
    octopusOs.driver_host_slots = driverSlots;
  }
  config.octopus_os = octopusOs;
  next.config = config;
}

async function configureOctopusOsProjectDefaults(
  registryRoot,
  octopusHomeRoot,
  scope,
  request,
  commitClientAuthoring,
) {
  if (typeof commitClientAuthoring !== "function") {
    throw new Error("client authoring transaction is unavailable");
  }
  const entries = Array.isArray(request?.entries) ? request.entries : null;
  if (!entries || entries.length === 0 || entries.length > 64) {
    throw new Error("configure_project_defaults requires 1 to 64 capability entries");
  }
  const seenCapabilities = new Set();
  const [allInterfaces, allTools, allKinds] = await Promise.all([
    installedDescriptorIds(path.join(registryRoot, "interface"), "interface.json", ["id", "rule_id"]),
    installedDescriptorIds(path.join(registryRoot, "tool"), "tool.json", ["id"]),
    installedDescriptorIds(path.join(registryRoot, "worker_environment_kind"), "worker_environment_kind.json", ["worker_environment_kind_id", "worker_environment_kind", "kind", "id"]),
  ]);
  const hasApplication = entries.some((entry) => stringValue(entry?.action) === "configure_application");
  const hasDriver = entries.some((entry) => stringValue(entry?.action) === "configure_driver");
  const interfaceRoot = hasApplication ? await fs.realpath(path.join(registryRoot, "interface")) : null;
  const kindRoot = hasDriver ? await fs.realpath(path.join(registryRoot, "worker_environment_kind")) : null;
  const toolRoot = hasDriver && allTools.length > 0 ? await fs.realpath(path.join(registryRoot, "tool")) : null;
  const applicationDocuments = hasApplication
    ? await readInstalledApplicationManifestDocuments(registryRoot)
    : [];
  const toolDocuments = new Map();
  if (hasDriver && toolRoot) {
    await Promise.all(allTools.map(async (toolId) => {
      const doc = (await readRegularJsonDocument(
        path.join(toolRoot, toolId, "tool.json"),
        `tool descriptor ${toolId}`,
      )).document;
      toolDocuments.set(toolId, doc);
    }));
  }

  const eeStates = new Map();
  const loadEeState = async (eeId, expectedDigest) => {
    const environment = scope.byId.get(eeId);
    if (!environment) throw new Error(`project EE '${eeId}' is outside the Octopus OS scope`);
    const digest = requiredDigest(expectedDigest, "expectedDigest");
    const existing = eeStates.get(eeId);
    if (existing) {
      if (existing.beforeDigest !== digest) {
        throw new Error(`project EE '${eeId}' changed before its capability setup could be applied`);
      }
      return existing;
    }
    const eePath = await scopedRecordPath(
      octopusHomeRoot,
      environment.record_path,
      path.join("state", "ee_instances"),
      `${eeId}.json`,
    );
    const current = await readRegularJsonDocument(eePath, `project EE ${eeId}`);
    if (current.digest !== digest || stringValue(environment.record_digest) !== digest) {
      throw new Error(`project EE '${eeId}' changed before its capability setup could be applied`);
    }
    if (stringValue(current.document.ee_id) !== eeId) {
      throw new Error(`project EE '${eeId}' has drifted identity`);
    }
    const state = {
      eePath,
      beforeDigest: digest,
      environment,
      next: JSON.parse(JSON.stringify(current.document)),
    };
    eeStates.set(eeId, state);
    return state;
  };

  const results = [];
  for (const entry of entries) {
    const action = stringValue(entry?.action);
    if (action === "configure_application") {
      const applicationId = requiredSafeRegistryId(entry?.applicationId, "applicationId");
      const key = `application:${applicationId}`;
      if (seenCapabilities.has(key)) throw new Error(`configure_project_defaults repeats application '${applicationId}'`);
      seenCapabilities.add(key);
      const projects = projectCapabilityChanges(entry, false);
      const applicationPath = path.join(interfaceRoot, applicationId, "application.json");
      const applicationDocument = await readRegularJsonDocument(applicationPath, `application manifest ${applicationId}`);
      const manifest = parseApplicationManifest(applicationDocument.document, applicationId, applicationPath);
      const requiredInterfaces = new Set(manifest.interfaces);
      const requiredTools = new Set();
      const requiredKinds = new Set();
      for (const capability of manifest.task_capabilities.tools) {
        requiredInterfaces.add(capability.interface);
        requiredTools.add(capability.tool);
        requiredKinds.add(capability.worker_environment_kind);
        await assertApplicationToolCapability(registryRoot, applicationId, capability);
      }
      for (const interfaceId of requiredInterfaces) {
        if (!allInterfaces.includes(interfaceId)) {
          throw new Error(`application '${applicationId}' references missing interface '${interfaceId}'`);
        }
      }
      const projectChanges = [];
      for (const project of projects) {
        const state = await loadEeState(project.eeId, project.expectedDigest);
        const retainedApplications = applicationDocuments.filter((candidate) => (
          candidate.manifest.id !== applicationId
          && projectConfiguredApplication(state.environment, candidate.manifest)
        ));
        const retainedInterfaces = new Set(retainedApplications.flatMap((candidate) => [
          ...candidate.manifest.interfaces,
          ...candidate.manifest.task_capabilities.tools.map((capability) => capability.interface),
        ]));
        const retainedTools = new Set(retainedApplications.flatMap((candidate) => (
          candidate.manifest.task_capabilities.tools.map((capability) => capability.tool)
        )));
        const retainedKinds = new Set(retainedApplications.flatMap((candidate) => (
          candidate.manifest.task_capabilities.tools.map((capability) => capability.worker_environment_kind)
        )));
        applyProjectCapabilityStateMutation({
          eeId: project.eeId,
          environment: state.environment,
          next: state.next,
          capabilityId: applicationId,
          available: project.available,
          selectedByDefault: project.selectedByDefault,
          axisUpdates: [
            { field: "allowed_interfaces", values: [...requiredInterfaces], installedValues: allInterfaces, removeValues: [...requiredInterfaces].filter((value) => !retainedInterfaces.has(value)) },
            { field: "allowed_tools", values: [...requiredTools], installedValues: allTools, removeValues: [...requiredTools].filter((value) => !retainedTools.has(value)) },
            { field: "allowed_worker_environment_kinds", values: [...requiredKinds], installedValues: allKinds, removeValues: [...requiredKinds].filter((value) => !retainedKinds.has(value)) },
          ],
          defaultField: "application_defaults",
        });
        projectChanges.push({ eeId: project.eeId, available: project.available, selectedByDefault: project.selectedByDefault });
      }
      results.push({ action: "configure_application", capabilityId: applicationId, projectChanges });
    } else if (action === "configure_driver") {
      const driverName = requiredSafeRegistryId(entry?.driverName, "driverName");
      const key = `driver:${driverName}`;
      if (seenCapabilities.has(key)) throw new Error(`configure_project_defaults repeats driver '${driverName}'`);
      seenCapabilities.add(key);
      const projects = projectCapabilityChanges(entry, true);
      const descriptorPath = path.join(kindRoot, driverName, "worker_environment_kind.json");
      const descriptor = (await readRegularJsonDocument(descriptorPath, `worker environment kind ${driverName}`)).document;
      const declaredName = stringValue(
        descriptor.worker_environment_kind_id
        ?? descriptor.worker_environment_kind
        ?? descriptor.kind
        ?? descriptor.id,
      );
      if (declaredName !== driverName || stringValue(descriptor.worker_consent) !== "explicit_ceiling") {
        throw new Error(`driver '${driverName}' does not satisfy the current worker-environment-kind contract`);
      }
      const doorDriverRefs = arrayValue(descriptor.door_driver_refs)
        .map((value) => requiredSafeRegistryId(value, `driver '${driverName}' door driver ref`));
      if (doorDriverRefs.length === 0 || new Set(doorDriverRefs).size !== doorDriverRefs.length) {
        throw new Error(`driver '${driverName}' has no unique door driver refs`);
      }
      const toolIds = [];
      const doorInterfaces = new Set();
      for (const toolId of allTools) {
        const tool = toolDocuments.get(toolId);
        if (arrayValue(tool?.supported_worker_environment_kinds).map(stringValue).includes(driverName)) {
          toolIds.push(toolId);
          doorInterfaces.add(requiredSafeRegistryId(tool.interface, `tool '${toolId}' owner interface`));
        }
      }
      if (doorInterfaces.size === 0) {
        throw new Error(`driver '${driverName}' has no tool-owned door interface`);
      }
      const projectChanges = [];
      for (const project of projects) {
        const state = await loadEeState(project.eeId, project.expectedDigest);
        applyProjectCapabilityStateMutation({
          eeId: project.eeId,
          environment: state.environment,
          next: state.next,
          capabilityId: driverName,
          available: project.available,
          selectedByDefault: project.selectedByDefault,
          axisUpdates: [
            { field: "allowed_worker_environment_kinds", values: [driverName], installedValues: allKinds, removeValues: [driverName] },
            { field: "allowed_interfaces", values: [...doorInterfaces], installedValues: allInterfaces },
            { field: "allowed_tools", values: toolIds, installedValues: allTools },
          ],
          defaultField: "driver_defaults",
          driverHostSlots: parseDriverHostSlots(project.hostSlots, descriptor, driverName),
        });
        projectChanges.push({ eeId: project.eeId, available: project.available, selectedByDefault: project.selectedByDefault });
      }
      results.push({ action: "configure_driver", capabilityId: driverName, projectChanges });
    } else {
      throw new Error(`configure_project_defaults contains invalid entry action '${action}'`);
    }
  }
  const writes = [...eeStates.values()].map((state) => ({
    file: state.eePath,
    beforeDigest: state.beforeDigest,
    next: state.next,
  }));
  await commitClientAuthoring({
    label: `configure_project_defaults (${entries.length})`,
    writes,
    removals: [],
    publishBarrierPath: writes.at(-1)?.file,
  });
  return {
    action: "configure_project_defaults",
    results,
  };
}

function assertCapabilityValuesWithinCeiling(ceiling, field, values, capabilityId) {
  const allowed = ceiling[field];
  if (!Array.isArray(allowed)) return;
  const missing = values.filter((value) => !allowed.includes(value));
  if (missing.length) {
    throw new Error(`capability '${capabilityId}' exceeds the project ${field} ceiling: ${missing.join(", ")}`);
  }
}

function parseDriverHostSlots(value, descriptor, driverName) {
  if (value === undefined) return {};
  if (!isPlainRecord(value)) throw new Error(`driver '${driverName}' hostSlots must be an object`);
  const boundary = objectOrEmpty(descriptor.boundary_defaults);
  const declared = new Set([
    ...arrayValue(boundary.read_root_refs),
    ...arrayValue(boundary.write_root_refs),
    ...arrayValue(boundary.protected_read_only_subpaths),
  ].map((entry) => stringValue(entry?.path)).filter(Boolean));
  const result = {};
  for (const [slot, slotPath] of Object.entries(value)) {
    if (!declared.has(slot) || typeof slotPath !== "string" || !path.isAbsolute(slotPath)) {
      throw new Error(`driver '${driverName}' host slot '${slot}' is not a declared absolute path`);
    }
    result[slot] = path.normalize(slotPath);
  }
  return result;
}

async function installedDescriptorIds(root, fileName, identityFields) {
  const entries = await readDirectoryEntries(root);
  const ids = [];
  for (const entry of entries) {
    if (!entry.isDirectory() || !safeInterfacePathSegment(entry.name)) continue;
    const descriptor = await readOptionalRegularJsonDocument(
      path.join(root, entry.name, fileName),
      `${fileName} descriptor ${entry.name}`,
    );
    if (!descriptor) continue;
    const identity = identityFields.map((field) => stringValue(descriptor.document[field])).find(Boolean);
    if (identity !== entry.name) throw new Error(`${fileName} descriptor '${entry.name}' has drifted identity`);
    ids.push(entry.name);
  }
  return ids.sort();
}
