module.exports = { clientAgentTemplateFromRequest, createOctopusOsAgentTemplate, mutateOctopusOsAgentTemplate, planAgentWorkerAdmission, requiredAgentIcon, requiredAgentId, requiredAgentModel, requiredAgentName, sourceProfileForWorker };

const fs = require("node:fs/promises");
const crypto = require("node:crypto");
const path = require("node:path");
const { clientAgentCatalogDigest, clientAgentCatalogPath, parseClientAgentCatalog, readClientAgentCatalog, requiredClientAgentIcon, requiredClientAgentId, requiredClientAgentName, planClientAgentTemplate, requireAgentSourceAdmission } = require("../clientAgentCatalog.cjs");
const { jsonDocumentDigest, readOptionalRegularJsonDocument, readRegularJsonDocument, requirePlainDirectory, requireUnchangedJsonDigest, stableJson } = require("./jsonFiles.cjs");
const { applicationWithinWorkerCeiling } = require("./operatorChatContext.cjs");
const { parseClientWorkerTemplate, parseOctopusOsSidecar, parseUniqueRegistryIds } = require("./sidecars.cjs");
const { scopeEnvironmentChain, scopedRecordPath } = require("./taskDeletion.cjs");
const { arrayValue, isPlainRecord, objectOrEmpty, requiredAbsolutePath, requiredApplicationOperationId, requiredDigest, requiredSafeRegistryId, requiredString, stringValue } = require("./values.cjs");

async function createOctopusOsAgentTemplate(
  registryRoot,
  octopusHomeRoot,
  scope,
  request,
  commitClientAuthoring,
  chatContexts,
  preset = null,
) {
  if (scope.status !== "ready") throw new Error("Agent creation requires a ready Octopus OS Home surface");
  if (typeof commitClientAuthoring !== "function") {
    throw new Error("client authoring transaction is unavailable");
  }
  const operationId = requiredApplicationOperationId(request?.operationId);
  const projectEeId = requiredSafeRegistryId(request?.projectEeId, "Agent Project EE");
  const sourceWorkerInterfaceId = requiredSafeRegistryId(
    request?.sourceWorkerInterfaceId,
    "Agent source worker template",
  );
  if (!scope.byId.has(projectEeId)) {
    throw new Error(`Agent EE '${projectEeId}' is outside the workspace scope`);
  }
  const requestedWorkspace = objectOrEmpty(request?.workspace);
  if (
    requiredSafeRegistryId(requestedWorkspace.rootEeId, "Agent workspace root EE") !== scope.rootEeId
    || requiredSafeRegistryId(requestedWorkspace.homeRunId, "Agent workspace Home run") !== scope.homeRunId
    || requiredSafeRegistryId(requestedWorkspace.homeInterfaceId, "Agent workspace Home interface") !== scope.homeInterfaceId
  ) {
    throw new Error("Agent creation does not match the projected Octopus OS workspace");
  }
  const expectedHomeSidecarDigest = requiredDigest(
    requestedWorkspace.expectedHomeSidecarDigest,
    "Agent workspace Home sidecar digest",
  );
  const expectedAgentCatalogDigest = requiredDigest(
    requestedWorkspace.expectedAgentCatalogDigest,
    "Agent workspace catalog digest",
  );
  if (
    expectedAgentCatalogDigest !== scope.agentCatalogDigest
    || requiredAbsolutePath(scope.agentCatalogPath, "Agent workspace catalog path")
      !== clientAgentCatalogPath(octopusHomeRoot)
  ) {
    throw new Error("Agent creation does not match the projected Client Agent catalog");
  }
  const projectedSources = scope.agentProjectSources.filter((source) => (
    stringValue(source?.project_ee_id) === projectEeId
  ));
  if (
    !preset && (projectedSources.length !== 1
    || stringValue(projectedSources[0]?.source_worker_interface) !== sourceWorkerInterfaceId
    )
  ) {
    throw new Error(`Project '${projectEeId}' does not name '${sourceWorkerInterfaceId}' as its exact Agent source`);
  }
  const sourceProfiles = scope.taskAuthoringProfiles.filter((profile) => (
    stringValue(profile?.ee_id) === projectEeId
    && stringValue(profile?.worker_choice_interface) === sourceWorkerInterfaceId
  ));
  if (!preset && sourceProfiles.length !== 1) {
    throw new Error(`Project '${projectEeId}' exposes ${sourceProfiles.length} exact source profiles for '${sourceWorkerInterfaceId}'`);
  }
  const clientTemplate = preset?.settings ?? clientAgentTemplateFromRequest(
    request,
    chatContexts,
    projectEeId,
    sourceWorkerInterfaceId,
  );
  const registryHome = await fs.realpath(registryRoot);
  const interfaceRoot = await fs.realpath(path.join(registryHome, "interface"));
  if (path.dirname(interfaceRoot) !== registryHome || path.basename(interfaceRoot) !== "interface") {
    throw new Error("active interface registry is outside the active Registry");
  }
  const homeSidecarPath = path.join(interfaceRoot, scope.homeInterfaceId, "octopus_os.json");
  const homeSidecar = await readRegularJsonDocument(homeSidecarPath, "Octopus OS Home sidecar");
  const parsedHomeSidecar = parseOctopusOsSidecar(
    homeSidecar.document,
    scope.homeInterfaceId,
    homeSidecarPath,
  );
  const sourceProfile = preset ? [preset.profile] : arrayValue(parsedHomeSidecar.task_authoring_profiles).filter((profile) => (
    stringValue(profile?.ee_id) === projectEeId
    && stringValue(profile?.worker_choice_interface) === sourceWorkerInterfaceId
  ));
  if (sourceProfile.length !== 1) {
    throw new Error(`Home sidecar no longer exposes one source profile for '${sourceWorkerInterfaceId}' in '${projectEeId}'`);
  }
  const agentCatalog = await readClientAgentCatalog(octopusHomeRoot);
  if (
    !agentCatalog.exists
    || agentCatalog.digest !== expectedAgentCatalogDigest
    || agentCatalog.file !== scope.agentCatalogPath
  ) {
    throw new Error("Client Agent catalog changed before the Agent template could be created");
  }
  const workerInterfaceId = `system_worker_template_${crypto.createHash("sha256")
    .update(`${operationId}\0${projectEeId}\0${sourceWorkerInterfaceId}`)
    .digest("hex")
    .slice(0, 32)}`;
  const agentId = `agent_${crypto.createHash("sha256")
    .update(`${operationId}\0${workerInterfaceId}`)
    .digest("hex")
    .slice(0, 32)}`;
  const {
    interfacePath: sourceInterfacePath,
    policyPath: sourcePolicyPath,
    interface: sourceInterface,
    policy: sourcePolicy,
  } = await readSourceWorkerTemplate(interfaceRoot, sourceWorkerInterfaceId);
  const targetDirectory = path.join(interfaceRoot, workerInterfaceId);
  const targetInterfacePath = path.join(targetDirectory, "interface.json");
  const targetPolicyPath = path.join(targetDirectory, "policy.json");
  const targetClientTemplatePath = path.join(targetDirectory, "client_template.json");
  let targetDirectoryExists = false;
  try {
    const stat = await fs.lstat(targetDirectory);
    if (stat.isSymbolicLink() || !stat.isDirectory()) {
      throw new Error(`Agent worker template '${workerInterfaceId}' is not a plain directory`);
    }
    targetDirectoryExists = true;
  } catch (error) {
    if (error?.code !== "ENOENT") throw error;
  }
  const existingInterface = await readOptionalRegularJsonDocument(targetInterfacePath, `Agent worker descriptor ${workerInterfaceId}`);
  const existingPolicy = await readOptionalRegularJsonDocument(targetPolicyPath, `Agent worker policy ${workerInterfaceId}`);
  const existingClientTemplate = await readOptionalRegularJsonDocument(targetClientTemplatePath, `Agent client template ${workerInterfaceId}`);
  const existingProfile = sourceProfileForWorker(parsedHomeSidecar.task_authoring_profiles, projectEeId, workerInterfaceId);
  const existingPresentation = arrayValue(agentCatalog.document.agents).find((agent) => (
    stringValue(agent?.project_ee_id) === projectEeId
    && stringValue(agent?.worker_interface) === workerInterfaceId
  ));
  if (targetDirectoryExists || existingInterface || existingPolicy || existingClientTemplate || existingProfile || existingPresentation) {
    if (!existingInterface || !existingPolicy || !existingClientTemplate || !existingProfile || !existingPresentation) {
      throw new Error(`Agent worker template '${workerInterfaceId}' is only partially authored`);
    }
    const expectedInterface = { ...structuredClone(sourceInterface.document), id: workerInterfaceId, copy_of: sourceInterface.document.copy_of ?? sourceInterface.document.id };
    const expectedPolicy = { ...structuredClone(sourcePolicy.document), id: workerInterfaceId };
    const expectedProfile = { ...structuredClone(sourceProfile[0]), worker_choice_interface: workerInterfaceId };
    const expectedClientTemplate = {
      ...clientTemplate,
      worker_interface: workerInterfaceId,
    };
    const expectedPresentation = {
      ...existingPresentation,
      id: agentId,
      name: requiredAgentName(request?.name),
      icon: requiredAgentIcon(request?.icon),
      project_ee_id: projectEeId,
      worker_interface: workerInterfaceId,
      deleted_at_ms: 0,
    };
    if (
      stableJson(existingInterface.document) !== stableJson(expectedInterface)
      || stableJson(existingPolicy.document) !== stableJson(expectedPolicy)
      || stableJson(existingClientTemplate.document) !== stableJson(expectedClientTemplate)
      || stableJson(existingProfile) !== stableJson(expectedProfile)
      || stableJson(existingPresentation) !== stableJson(expectedPresentation)
    ) {
      throw new Error(`Agent worker template '${workerInterfaceId}' belongs to another authoring operation`);
    }
    return {
      action: "create_agent_template",
      eeId: projectEeId,
      workerInterfaceId,
      agentId,
      clientTemplateDigest: existingClientTemplate.digest,
      agentCatalogDigest: agentCatalog.digest,
      changed: false,
    };
  }
  if (
    homeSidecar.digest !== expectedHomeSidecarDigest
    || homeSidecar.digest !== scope.homeSidecarDigest
  ) {
    throw new Error("Octopus OS Home changed before the Agent template could be created");
  }
  const nextInterface = { ...structuredClone(sourceInterface.document), id: workerInterfaceId, copy_of: sourceInterface.document.copy_of ?? sourceInterface.document.id };
  const nextPolicy = { ...structuredClone(sourcePolicy.document), id: workerInterfaceId };
  const nextClientTemplate = { ...clientTemplate, worker_interface: workerInterfaceId };
  const materialized = planClientAgentTemplate({
    operationId, projectEeId, sourceInterface: sourceInterface.document, sourcePolicy: sourcePolicy.document,
    sourceProfile: sourceProfile[0], settings: clientTemplate, name: request.name, icon: request.icon,
  });
  const nextProfile = materialized.profile;
  const nextHomeSidecar = structuredClone(homeSidecar.document);
  nextHomeSidecar.task_authoring_profiles.push(nextProfile);
  parseOctopusOsSidecar(nextHomeSidecar, scope.homeInterfaceId, homeSidecarPath);
  const nextAgentCatalog = structuredClone(agentCatalog.document);
  nextAgentCatalog.agents.push(materialized.presentation);
  parseClientAgentCatalog(nextAgentCatalog, agentCatalog.file);
  const admissionWrites = await planAgentWorkerAdmission(
    registryRoot,
    octopusHomeRoot,
    scope,
    projectEeId,
    sourceWorkerInterfaceId,
    workerInterfaceId,
  );
  await requireUnchangedJsonDigest(sourceInterfacePath, sourceInterface.digest, `source worker descriptor ${sourceWorkerInterfaceId}`);
  await requireUnchangedJsonDigest(sourcePolicyPath, sourcePolicy.digest, `source worker policy ${sourceWorkerInterfaceId}`);
  if (preset) await preset.verify();
  await commitClientAuthoring({
    label: `create Agent worker template ${workerInterfaceId}`,
    writes: [
      { file: targetInterfacePath, beforeDigest: null, next: nextInterface },
      { file: targetPolicyPath, beforeDigest: null, next: nextPolicy },
      { file: targetClientTemplatePath, beforeDigest: null, next: nextClientTemplate },
      { file: homeSidecarPath, beforeDigest: homeSidecar.digest, next: nextHomeSidecar },
      ...admissionWrites,
      { file: agentCatalog.file, beforeDigest: agentCatalog.digest, next: nextAgentCatalog },
    ],
    removals: [],
    publishBarrierPath: agentCatalog.file,
  });
  return {
    action: "create_agent_template",
    eeId: projectEeId,
    workerInterfaceId,
    agentId,
    clientTemplateDigest: jsonDocumentDigest(nextClientTemplate),
    agentCatalogDigest: clientAgentCatalogDigest(nextAgentCatalog),
    changed: true,
  };
}

async function mutateOctopusOsAgentTemplate(
  registryRoot,
  octopusHomeRoot,
  scope,
  request,
  commitClientAuthoring,
  chatContexts,
) {
  if (scope.status !== "ready") throw new Error("Agent editing requires a ready Octopus OS Home surface");
  if (typeof commitClientAuthoring !== "function") {
    throw new Error("client authoring transaction is unavailable");
  }
  const action = stringValue(request?.action);
  const projectEeId = requiredSafeRegistryId(request?.projectEeId, "Agent Project EE");
  const workerInterfaceId = requiredSafeRegistryId(request?.workerInterfaceId, "Agent worker template");
  const agentId = requiredAgentId(request?.agentId);
  if (!scope.byId.has(projectEeId)) {
    throw new Error(`Agent EE '${projectEeId}' is outside the workspace scope`);
  }
  const profiles = scope.taskAuthoringProfiles.filter((profile) => (
    stringValue(profile?.ee_id) === projectEeId
    && stringValue(profile?.worker_choice_interface) === workerInterfaceId
  ));
  if (profiles.length !== 1) {
    throw new Error(`Project '${projectEeId}' exposes ${profiles.length} exact profiles for '${workerInterfaceId}'`);
  }
  const expectedAgentCatalogDigest = requiredDigest(
    request?.expectedAgentCatalogDigest,
    "Agent catalog digest",
  );
  if (expectedAgentCatalogDigest !== scope.agentCatalogDigest) {
    throw new Error("Client Agent catalog changed before the Agent could be edited");
  }
  const agentCatalog = await readClientAgentCatalog(octopusHomeRoot);
  if (
    !agentCatalog.exists
    || agentCatalog.digest !== expectedAgentCatalogDigest
    || agentCatalog.file !== scope.agentCatalogPath
  ) {
    throw new Error("Client Agent catalog changed before the Agent could be edited");
  }
  const presentationMatches = agentCatalog.document.agents.filter((agent) => (
    agent.id === agentId
    && agent.project_ee_id === projectEeId
    && agent.worker_interface === workerInterfaceId
  ));
  if (presentationMatches.length !== 1) {
    throw new Error(`Client Agent catalog exposes ${presentationMatches.length} exact presentations for '${agentId}'`);
  }
  const presentation = presentationMatches[0];
  if (presentation.deleted_at_ms > 0) {
    throw new Error(`Agent '${agentId}' is deleted`);
  }
  const interfaceRoot = await fs.realpath(path.join(await fs.realpath(registryRoot), "interface"));
  const workerDirectory = path.join(interfaceRoot, workerInterfaceId);
  await requirePlainDirectory(workerDirectory, `Agent worker template ${workerInterfaceId}`);
  const templatePath = path.join(workerDirectory, "client_template.json");
  const current = await readRegularJsonDocument(templatePath, `Agent client template ${workerInterfaceId}`);
  const parsed = parseClientWorkerTemplate(current.document, workerInterfaceId, templatePath);
  if (parsed.project_ee_id !== projectEeId) {
    throw new Error(`Agent worker template '${workerInterfaceId}' belongs to another Project`);
  }
  const expectedDigest = requiredDigest(request?.expectedClientTemplateDigest, "Agent client template digest");
  if (current.digest !== expectedDigest) {
    throw new Error(`Agent worker template '${workerInterfaceId}' changed before it could be edited`);
  }
  if (action === "delete_agent_template") {
    const now = Date.now();
    const nextAgentCatalog = structuredClone(agentCatalog.document);
    const presentationIndex = nextAgentCatalog.agents.findIndex((agent) => agent.id === agentId);
    nextAgentCatalog.agents[presentationIndex] = {
      ...nextAgentCatalog.agents[presentationIndex],
      updated_at_ms: now,
      deleted_at_ms: now,
    };
    parseClientAgentCatalog(nextAgentCatalog, agentCatalog.file);
    await commitClientAuthoring({
      label: `delete Agent client template ${workerInterfaceId}`,
      writes: [
        { file: templatePath, beforeDigest: current.digest, next: null },
        { file: agentCatalog.file, beforeDigest: agentCatalog.digest, next: nextAgentCatalog },
      ],
      removals: [],
      publishBarrierPath: agentCatalog.file,
    });
    return {
      action,
      eeId: projectEeId,
      workerInterfaceId,
      agentId,
      agentCatalogDigest: clientAgentCatalogDigest(nextAgentCatalog),
      changed: true,
    };
  }
  const next = {
    ...clientAgentTemplateFromRequest(
      request,
      chatContexts,
      projectEeId,
      workerInterfaceId,
    ),
    worker_interface: workerInterfaceId,
  };
  const nextName = requiredAgentName(request?.name);
  const nextIcon = requiredAgentIcon(request?.icon);
  const agentInterfacePath = path.join(workerDirectory, "interface.json");
  const agentPolicyPath = path.join(workerDirectory, "policy.json");
  const agentInterface = await readRegularJsonDocument(agentInterfacePath, `Agent worker descriptor ${workerInterfaceId}`);
  const agentPolicy = await readRegularJsonDocument(agentPolicyPath, `Agent worker policy ${workerInterfaceId}`);
  if (
    stringValue(agentInterface.document?.id) !== workerInterfaceId
    || stringValue(agentPolicy.document?.id) !== workerInterfaceId
  ) {
    throw new Error(`Agent worker template '${workerInterfaceId}' is malformed`);
  }
  const sourceWorkerInterfaceId = requiredSafeRegistryId(
    agentInterface.document?.copy_of,
    `Agent worker template '${workerInterfaceId}' source`,
  );
  const source = await readSourceWorkerTemplate(interfaceRoot, sourceWorkerInterfaceId);
  const nextPolicy = { ...structuredClone(source.policy.document), id: workerInterfaceId };
  const policyChanged = stableJson(agentPolicy.document) !== stableJson(nextPolicy);
  const applicationCatalog = new Map(arrayValue(chatContexts?.application_catalog).map((application) => [stringValue(application?.id), application]));
  const outsidePin = [
    ...[next.provider_interface_id].filter((id) => !arrayValue(nextPolicy.ceiling.interfaces).includes(id)),
    ...next.application_ids.filter((id) => !applicationCatalog.has(id) || !applicationWithinWorkerCeiling(applicationCatalog.get(id), nextPolicy.ceiling)),
    ...next.worker_environment_kinds.filter((kind) => !arrayValue(nextPolicy.ceiling.wek).includes(kind)),
  ];
  if (outsidePin.length) {
    throw new Error(`Agent selection is outside source worker template '${sourceWorkerInterfaceId}': ${outsidePin.join(", ")}`);
  }
  if (stableJson(nextPolicy.ceiling.interface_policy ?? null) !== stableJson(agentPolicy.document.ceiling?.interface_policy ?? null)) {
    throw new Error(`source worker template '${sourceWorkerInterfaceId}' changed its provider interface policy; create a new Agent from it to choose a model under that policy`);
  }
  if (
    stableJson(current.document) === stableJson(next)
    && presentation.name === nextName
    && presentation.icon === nextIcon
    && !policyChanged
  ) {
    return {
      action,
      eeId: projectEeId,
      workerInterfaceId,
      agentId,
      clientTemplateDigest: current.digest,
      agentCatalogDigest: agentCatalog.digest,
      changed: false,
    };
  }
  const nextAgentCatalog = structuredClone(agentCatalog.document);
  const presentationIndex = nextAgentCatalog.agents.findIndex((agent) => agent.id === agentId);
  nextAgentCatalog.agents[presentationIndex] = {
    ...nextAgentCatalog.agents[presentationIndex],
    name: nextName,
    icon: nextIcon,
    updated_at_ms: Date.now(),
  };
  parseClientAgentCatalog(nextAgentCatalog, agentCatalog.file);
  await requireUnchangedJsonDigest(agentInterfacePath, agentInterface.digest, `Agent worker descriptor ${workerInterfaceId}`);
  await requireUnchangedJsonDigest(source.interfacePath, source.interface.digest, `source worker descriptor ${sourceWorkerInterfaceId}`);
  await requireUnchangedJsonDigest(source.policyPath, source.policy.digest, `source worker policy ${sourceWorkerInterfaceId}`);
  await commitClientAuthoring({
    label: `configure Agent client template ${workerInterfaceId}`,
    writes: [
      { file: templatePath, beforeDigest: current.digest, next },
      ...(policyChanged ? [{ file: agentPolicyPath, beforeDigest: agentPolicy.digest, next: nextPolicy }] : []),
      { file: agentCatalog.file, beforeDigest: agentCatalog.digest, next: nextAgentCatalog },
    ],
    removals: [],
    publishBarrierPath: agentCatalog.file,
  });
  return {
    action,
    eeId: projectEeId,
    workerInterfaceId,
    agentId,
    clientTemplateDigest: jsonDocumentDigest(next),
    agentCatalogDigest: clientAgentCatalogDigest(nextAgentCatalog),
    changed: true,
  };
}

async function readSourceWorkerTemplate(interfaceRoot, sourceWorkerInterfaceId) {
  const sourceDirectory = path.join(interfaceRoot, sourceWorkerInterfaceId);
  await requirePlainDirectory(sourceDirectory, `source worker template ${sourceWorkerInterfaceId}`);
  const interfacePath = path.join(sourceDirectory, "interface.json");
  const policyPath = path.join(sourceDirectory, "policy.json");
  const sourceInterface = await readRegularJsonDocument(interfacePath, `source worker descriptor ${sourceWorkerInterfaceId}`);
  const sourcePolicy = await readRegularJsonDocument(policyPath, `source worker policy ${sourceWorkerInterfaceId}`);
  if (
    stringValue(sourceInterface.document?.id) !== sourceWorkerInterfaceId
    || stringValue(sourceInterface.document?.interface_type).toLowerCase() !== "system"
    || stringValue(sourceInterface.document?.role).toLowerCase() !== "worker"
    || stringValue(sourcePolicy.document?.id) !== sourceWorkerInterfaceId
    || !isPlainRecord(sourcePolicy.document?.ceiling)
  ) {
    throw new Error(`source worker template '${sourceWorkerInterfaceId}' is malformed`);
  }
  return { interfacePath, policyPath, interface: sourceInterface, policy: sourcePolicy };
}

function clientAgentTemplateFromRequest(request, chatContexts, projectEeId, workerInterfaceId) {
  const context = arrayValue(chatContexts?.contexts).find((candidate) => (
    stringValue(candidate?.ee_id) === projectEeId
  ));
  const worker = arrayValue(context?.workers).find((candidate) => (
    stringValue(candidate?.id) === workerInterfaceId
    && candidate?.authoring_profile === true
  ));
  if (!worker) {
    throw new Error(`Agent source worker '${workerInterfaceId}' is not projected in Project '${projectEeId}'`);
  }
  const providerInterfaceId = requiredSafeRegistryId(request?.providerInterfaceId, "Agent provider interface");
  const providerIds = new Set(arrayValue(worker.provider_interface_ids).map(stringValue).filter(Boolean));
  if (!providerIds.has(providerInterfaceId)) {
    throw new Error(`Agent provider '${providerInterfaceId}' is not admitted by worker template '${workerInterfaceId}'`);
  }
  const providerModel = requiredAgentModel(request?.providerModel);
  const admittedModels = worker.interface_policy?.[providerInterfaceId]?.models;
  if (!Array.isArray(admittedModels) || !admittedModels.includes(providerModel)) {
    throw new Error(`Agent model '${providerModel}' is not admitted by worker template '${workerInterfaceId}'`);
  }
  const reasoningEffort = stringValue(request?.reasoningEffort);
  const serviceTier = request?.serviceTier === undefined || request.serviceTier === "" ? "" : requiredString(request.serviceTier, "Agent service tier");
  if (serviceTier.length > 80 || /[\u0000-\u001f\u007f]/.test(serviceTier)) throw new Error("Agent service tier is invalid");
  if (reasoningEffort.length > 80 || /[\u0000-\u001f\u007f]/.test(reasoningEffort)) {
    throw new Error("Agent reasoning effort is invalid");
  }
  const applicationIds = parseUniqueRegistryIds(request?.applicationIds, "Agent application").sort();
  const availableApplicationIds = new Set(arrayValue(worker.applications).map((application) => stringValue(application?.id)).filter(Boolean));
  const unavailableApplicationIds = applicationIds.filter((id) => !availableApplicationIds.has(id));
  if (unavailableApplicationIds.length) {
    throw new Error(`Agent applications are not admitted by the worker template: ${unavailableApplicationIds.join(", ")}`);
  }
  const workerEnvironmentKinds = parseUniqueRegistryIds(request?.driverNames, "Agent worker environment").sort();
  const availableWorkerEnvironmentKinds = new Set(arrayValue(worker.drivers).map((driver) => stringValue(driver?.name)).filter(Boolean));
  const unavailableWorkerEnvironmentKinds = workerEnvironmentKinds.filter((id) => !availableWorkerEnvironmentKinds.has(id));
  if (unavailableWorkerEnvironmentKinds.length) {
    throw new Error(`Agent Worker Environments are not admitted by the worker template: ${unavailableWorkerEnvironmentKinds.join(", ")}`);
  }
  return {
    schema_version: 1,
    worker_interface: workerInterfaceId,
    project_ee_id: projectEeId,
    provider_interface_id: providerInterfaceId,
    provider_model: providerModel,
    reasoning_effort: reasoningEffort,
    ...(serviceTier ? { service_tier: serviceTier } : {}),
    application_ids: applicationIds,
    worker_environment_kinds: workerEnvironmentKinds,
  };
}

function requiredAgentModel(value) {
  if (typeof value !== "string" || !value || value.trim() !== value || value.length > 512 || /[\u0000-\u001f\u007f]/.test(value)) {
    throw new Error("Choose an explicit Agent provider model.");
  }
  return value;
}

function requiredAgentId(value) {
  return requiredClientAgentId(value, "Agent id");
}

function requiredAgentName(value) {
  return requiredClientAgentName(value, "Agent name");
}

function requiredAgentIcon(value) {
  return requiredClientAgentIcon(value, "Agent icon");
}

async function planAgentWorkerAdmission(
  registryRoot,
  octopusHomeRoot,
  scope,
  projectEeId,
  sourceWorkerInterfaceId,
  workerInterfaceId,
) {
  const chain = scopeEnvironmentChain(projectEeId, scope.byId, scope.rootEeId);
  const registryHome = await fs.realpath(registryRoot);
  const templateRoot = await fs.realpath(path.join(registryHome, "ee_template"));
  if (path.dirname(templateRoot) !== registryHome || path.basename(templateRoot) !== "ee_template") {
    throw new Error("active EE template registry is outside the active Registry");
  }
  const templatePlans = new Map();
  const instanceWrites = [];
  for (const environment of chain) {
    const eeId = requiredSafeRegistryId(environment.ee_id, "Agent admission EE");
    const templateName = requiredSafeRegistryId(environment.template_name, `Agent admission EE '${eeId}' template`);
    if (!templatePlans.has(templateName)) {
      const file = path.join(templateRoot, templateName, "ee_template.json");
      const current = await readRegularJsonDocument(file, `Agent admission EE template ${templateName}`);
      if (stringValue(current.document?.id ?? current.document?.name) !== templateName) {
        throw new Error(`Agent admission EE template '${templateName}' has drifted identity`);
      }
      const next = structuredClone(current.document);
      requireAgentSourceAdmission(
        next,
        "allowed_interfaces",
        sourceWorkerInterfaceId,
        `EE template '${templateName}'`,
      );
      templatePlans.set(templateName, { file, current, next });
    }
    const recordPath = await scopedRecordPath(
      octopusHomeRoot,
      environment.record_path,
      path.join("state", "ee_instances"),
      `${eeId}.json`,
    );
    const current = await readRegularJsonDocument(recordPath, `Agent admission EE ${eeId}`);
    const projectedDigest = requiredDigest(environment.record_digest, `Agent admission EE '${eeId}' digest`);
    if (current.digest !== projectedDigest) {
      throw new Error(`Agent admission EE '${eeId}' changed before the worker template could be admitted`);
    }
    if (
      stringValue(current.document?.ee_id) !== eeId
      || stringValue(current.document?.template_name) !== templateName
    ) {
      throw new Error(`Agent admission EE '${eeId}' has drifted identity`);
    }
    const next = structuredClone(current.document);
    if (next.policy_narrowing !== undefined) {
      if (!isPlainRecord(next.policy_narrowing)) {
        throw new Error(`Agent admission EE '${eeId}'.policy_narrowing must be an object`);
      }
      requireAgentSourceAdmission(
        next.policy_narrowing,
        "allowed_interfaces",
        sourceWorkerInterfaceId,
        `EE '${eeId}' policy narrowing`,
      );
    }
    if (stableJson(next) !== stableJson(current.document)) {
      instanceWrites.push({ file: recordPath, beforeDigest: current.digest, next });
    }
  }
  return [
    ...[...templatePlans.values()].flatMap(({ file, current, next }) => (
      stableJson(next) === stableJson(current.document)
        ? []
        : [{ file, beforeDigest: current.digest, next }]
    )),
    ...instanceWrites,
  ];
}

function sourceProfileForWorker(profiles, eeId, workerInterfaceId) {
  const matches = arrayValue(profiles).filter((profile) => (
    stringValue(profile?.ee_id) === eeId
    && stringValue(profile?.worker_choice_interface) === workerInterfaceId
  ));
  if (matches.length > 1) throw new Error(`Home sidecar repeats worker template '${workerInterfaceId}' in '${eeId}'`);
  return matches[0] ?? null;
}
