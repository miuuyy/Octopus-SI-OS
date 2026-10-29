module.exports = { normalizeRuntimeLaunchEnvironment, parseApplicationManifest, parseApplicationSetup, parseApplicationTaskCapabilities, parseClientWorkerTemplate, parseOctopusOsSidecar, parseUniqueRegistryIds, resolveAuthoredApplicationSetupFields };

const { parseHomeVoiceContext } = require("../homeVoiceProjection.cjs");
const { requiredAgentModel } = require("./agentTemplates.cjs");
const { parseOctopusOsAuthoringProfile, parseOctopusOsPresentation, parseOctopusOsTaskAuthoringProfiles } = require("./authoringProfiles.cjs");
const { applicationLogoFileName } = require("./privateConfiguration.cjs");
const { arrayValue, exactObjectKeys, isPlainRecord, requiredBoolean, requiredContractToken, requiredSafeRegistryId, requiredString, safeInterfacePathSegment, stringValue, validJsonPointer } = require("./values.cjs");

function parseOctopusOsSidecar(value, interfaceId, file) {
  if (!isPlainRecord(value)) throw new Error(`Octopus OS sidecar is not an object: ${file}`);
  const allowedKeys = new Set([
    "schema_version",
    "workspace",
    "interface",
    "owner_interface",
    "component",
    "surface_role",
    "provider_choice_interface",
    "worker_choice_interface",
    "authoring_profile",
    "task_authoring_profiles",
    "presentation",
    "voice_context",
  ]);
  const unknown = Object.keys(value).filter((key) => !allowedKeys.has(key));
  const surfaceRole = stringValue(value.surface_role);
  const component = stringValue(value.component);
  if (
    unknown.length
    || value.schema_version !== 1
    || value.workspace !== "octopus_os"
    || value.interface !== interfaceId
    || !safeInterfacePathSegment(stringValue(value.owner_interface))
    || !["home", "task"].includes(surfaceRole)
    || !["client", "provider", "worker", "worker_environment_door", "authoring_source", "authoring_environment", "voice_control", "artifact_return"].includes(component)
  ) {
    throw new Error(`Octopus OS sidecar does not satisfy schema v1: ${file}`);
  }
  const providerChoice = stringValue(value.provider_choice_interface);
  const workerChoice = stringValue(value.worker_choice_interface);
  const authoringProfile = value.authoring_profile === undefined
    ? null
    : parseOctopusOsAuthoringProfile(value.authoring_profile, workerChoice, file);
  const taskAuthoringProfiles = value.task_authoring_profiles === undefined
    ? []
    : parseOctopusOsTaskAuthoringProfiles(value.task_authoring_profiles, file);
  const presentation = value.presentation === undefined
    ? null
    : parseOctopusOsPresentation(value.presentation, file);
  const voiceContext = value.voice_context == null ? null : parseHomeVoiceContext(value.voice_context);
  if (voiceContext && (component !== "client" || surfaceRole !== "home")) {
    throw new Error(`Octopus OS voice context must belong to the Home client: ${file}`);
  }
  if (component === "client") {
    if (value.owner_interface !== interfaceId || !safeInterfacePathSegment(providerChoice) || !safeInterfacePathSegment(workerChoice)) {
      throw new Error(`Octopus OS client sidecar has incomplete choice provenance: ${file}`);
    }
    if (!authoringProfile && (taskAuthoringProfiles.length || presentation)) {
      throw new Error(`Octopus OS client configuration has no authoring profile of its own: ${file}`);
    }
    if (presentation && surfaceRole === "task"
      && (!presentation.name || presentation.projects.length || /[\u0000-\u001f\u007f]/u.test(presentation.name))) {
      throw new Error(`Octopus OS task presentation must contain a name and no project presentations: ${file}`);
    }
    if (taskAuthoringProfiles.length && surfaceRole !== "home") {
      throw new Error(`Octopus OS task authoring profiles must belong to the Home client interface: ${file}`);
    }
  } else if (providerChoice || workerChoice || authoringProfile || taskAuthoringProfiles.length || presentation) {
    throw new Error(`Octopus OS non-client sidecar carries client choice fields: ${file}`);
  }
  return {
    ...value,
    authoring_profile: authoringProfile,
    task_authoring_profiles: taskAuthoringProfiles,
    presentation,
    descriptor_path: file,
  };
}

function parseClientWorkerTemplate(value, workerInterfaceId, file) {
  exactObjectKeys(
    value,
    [
      "schema_version",
      "worker_interface",
      "project_ee_id",
      "provider_interface_id",
      "provider_model",
      "reasoning_effort",
      "service_tier",
      "application_ids",
      "worker_environment_kinds",
    ],
    [
      "schema_version",
      "worker_interface",
      "project_ee_id",
      "provider_interface_id",
      "application_ids",
      "worker_environment_kinds",
    ],
    `client worker template ${file}`,
  );
  const workerInterface = requiredSafeRegistryId(value.worker_interface, "client worker template interface");
  const projectEeId = requiredSafeRegistryId(value.project_ee_id, "client worker template Project EE");
  const providerInterfaceId = requiredSafeRegistryId(value.provider_interface_id, "client worker template provider");
  const providerModel = value.provider_model === undefined ? "" : requiredAgentModel(value.provider_model);
  const reasoningEffort = stringValue(value.reasoning_effort);
  const serviceTier = value.service_tier === undefined ? "" : requiredString(value.service_tier, "client worker template service tier");
  if (serviceTier.length > 80 || /[\u0000-\u001f\u007f]/.test(serviceTier)) throw new Error("Agent service tier is invalid");
  if (
    value.schema_version !== 1
    || workerInterface !== workerInterfaceId
    || reasoningEffort.length > 80
    || /[\u0000-\u001f\u007f]/.test(reasoningEffort)
  ) {
    throw new Error(`client worker template does not satisfy schema v1: ${file}`);
  }
  const applicationIds = parseUniqueRegistryIds(value.application_ids, "client worker template application");
  const workerEnvironmentKinds = parseUniqueRegistryIds(
    value.worker_environment_kinds,
    "client worker template worker environment",
  );
  return {
    schema_version: 1,
    worker_interface: workerInterface,
    project_ee_id: projectEeId,
    provider_interface_id: providerInterfaceId,
    ...(providerModel ? { provider_model: providerModel } : {}),
    reasoning_effort: reasoningEffort,
    ...(serviceTier ? { service_tier: serviceTier } : {}),
    application_ids: applicationIds,
    worker_environment_kinds: workerEnvironmentKinds,
  };
}

function parseUniqueRegistryIds(value, label) {
  if (!Array.isArray(value)) throw new Error(`${label} ids must be an array`);
  const ids = value.map((candidate, index) => requiredSafeRegistryId(candidate, `${label} ${index}`));
  if (new Set(ids).size !== ids.length) throw new Error(`${label} ids must be unique`);
  return ids;
}

function parseApplicationManifest(value, interfaceId, file) {
  exactObjectKeys(
    value,
    [
      "schema_version",
      "id",
      "name",
      "description",
      "category",
      "developer",
      "version",
      "icon",
      "logo",
      "logo_dark",
      "interfaces",
      "artifact_bindings",
      "task_capabilities",
      "setup",
      "connection",
    ],
    ["schema_version", "id", "name", "description", "category", "developer", "version", "logo", "interfaces"],
    `application manifest ${file}`,
  );
  const id = requiredSafeRegistryId(value.id, "application manifest id");
  const name = stringValue(value.name);
  const description = stringValue(value.description);
  const category = stringValue(value.category);
  const developer = stringValue(value.developer);
  const version = stringValue(value.version);
  const icon = stringValue(value.icon);
  const logo = stringValue(value.logo);
  const logoDark = stringValue(value.logo_dark);
  if (
    value.schema_version !== 3
    || id !== interfaceId
    || !name
    || !description
    || !developer
    || !version
    || !["connector", "plugin", "artifact", "provider"].includes(category)
    || icon.length > 64
    || !applicationLogoFileName(logo)
    || (logoDark && !applicationLogoFileName(logoDark))
  ) {
    throw new Error(`application manifest does not satisfy schema v3: ${file}`);
  }
  if (
    !Array.isArray(value.interfaces)
    || value.interfaces.length === 0
    || new Set(value.interfaces).size !== value.interfaces.length
  ) {
    throw new Error(`application manifest interfaces are invalid: ${file}`);
  }
  const interfaces = value.interfaces.map((candidate, index) => (
    requiredSafeRegistryId(candidate, `application manifest interface ${index}`)
  ));
  if (!interfaces.includes(interfaceId)) {
    throw new Error(`application manifest does not include its owner interface: ${file}`);
  }
  if (value.artifact_bindings !== undefined && !Array.isArray(value.artifact_bindings)) {
    throw new Error(`application manifest artifact_bindings must be an array: ${file}`);
  }
  const artifactBindings = value.artifact_bindings === undefined
    ? []
    : value.artifact_bindings.map((candidate, index) => {
        exactObjectKeys(
          candidate,
          ["artifact_id", "label"],
          ["artifact_id"],
          `application manifest artifact binding ${index}`,
        );
        const artifactId = requiredSafeRegistryId(
          candidate.artifact_id,
          `application manifest artifact binding ${index} id`,
        );
        const label = stringValue(candidate.label);
        return { artifact_id: artifactId, ...(label ? { label } : {}) };
      });
  const taskCapabilities = parseApplicationTaskCapabilities(
    value.task_capabilities,
    file,
  );
  const setup = parseApplicationSetup(value.setup, interfaces, file);
  const connection = parseApplicationConnection(value.connection, file);
  return {
    schema_version: 3,
    id,
    name,
    description,
    category,
    developer,
    version,
    icon,
    logo,
    logo_dark: logoDark,
    interfaces,
    artifact_bindings: artifactBindings,
    task_capabilities: taskCapabilities,
    setup,
    connection,
  };
}

function parseApplicationConnection(value, file) {
  if (value === undefined) return { disclosure: [], help: null };
  exactObjectKeys(value, ["disclosure", "help"], [], `application manifest connection ${file}`);
  if (value.disclosure !== undefined && (!Array.isArray(value.disclosure) || value.disclosure.length > 4)) {
    throw new Error(`application manifest connection disclosure must list at most 4 statements: ${file}`);
  }
  const disclosure = (value.disclosure ?? []).map((statement, index) => (
    applicationPresentationText(statement, 160, `application connection disclosure ${index}`, file)
  ));
  let help = null;
  if (value.help !== undefined) {
    exactObjectKeys(value.help, ["label", "url"], ["label", "url"], `application manifest connection help ${file}`);
    help = {
      label: applicationPresentationText(value.help.label, 48, "application connection help label", file),
      url: applicationHttpsUrl(value.help.url, "application connection help url", file),
    };
  }
  return { disclosure, help };
}

function applicationPresentationText(value, limit, label, file) {
  if (typeof value !== "string" || !value.trim() || value.trim() !== value || value.length > limit || /[\u0000-\u001f\u007f]/u.test(value)) {
    throw new Error(`${label} must be plain text of 1..${limit} characters: ${file}`);
  }
  return value;
}

function applicationHttpsUrl(value, label, file) {
  let url;
  try {
    url = typeof value === "string" && value.length <= 4096 ? new URL(value) : null;
  } catch {
    url = null;
  }
  if (!url || url.protocol !== "https:" || url.username || url.password) {
    throw new Error(`${label} must be an https URL without credentials: ${file}`);
  }
  return url.href;
}

function parseApplicationTaskCapabilities(value, file) {
  if (value === undefined) return { tools: [] };
  exactObjectKeys(
    value,
    ["tools"],
    ["tools"],
    `application manifest task capabilities ${file}`,
  );
  if (!Array.isArray(value.tools)) {
    throw new Error(`application manifest task capability tools must be an array: ${file}`);
  }
  const tools = value.tools.map((candidate, index) => {
    exactObjectKeys(
      candidate,
      ["tool", "interface", "worker_environment_kind"],
      ["tool", "interface", "worker_environment_kind"],
      `application manifest task capability tool ${index}`,
    );
    return {
      tool: requiredContractToken(candidate.tool, `application task capability tool ${index}`),
      interface: requiredSafeRegistryId(candidate.interface, `application task capability tool ${index} interface`),
      worker_environment_kind: requiredSafeRegistryId(
        candidate.worker_environment_kind,
        `application task capability tool ${index} worker environment kind`,
      ),
    };
  });
  if (new Set(tools.map((tool) => tool.tool)).size !== tools.length) {
    throw new Error(`application manifest task capability tools contain duplicates: ${file}`);
  }
  return { tools };
}

function parseApplicationSetup(value, interfaces, file) {
  if (value === undefined) return { fields: [] };
  exactObjectKeys(value, ["fields"], ["fields"], `application manifest setup ${file}`);
  if (!Array.isArray(value.fields)) {
    throw new Error(`application manifest setup fields must be an array: ${file}`);
  }
  const interfaceIds = new Set(interfaces);
  const fields = value.fields.map((candidate, index) => {
    exactObjectKeys(
      candidate,
      ["interface", "pointer", "label", "kind", "required", "placeholder", "help"],
      ["interface", "pointer", "label", "kind", "required"],
      `application manifest setup field ${index}`,
    );
    const interfaceId = requiredSafeRegistryId(candidate.interface, `application setup field ${index} interface`);
    const pointer = requiredString(candidate.pointer, `application setup field ${index} pointer`);
    const label = requiredString(candidate.label, `application setup field ${index} label`);
    const kind = requiredString(candidate.kind, `application setup field ${index} kind`);
    if (!interfaceIds.has(interfaceId)) {
      throw new Error(`application setup field ${index} references an interface outside the package: ${file}`);
    }
    if (!validJsonPointer(pointer) || label.length > 96 || !["text", "url", "secret"].includes(kind)) {
      throw new Error(`application manifest setup field ${index} is invalid: ${file}`);
    }
    return {
      interface: interfaceId,
      pointer,
      label,
      kind,
      required: requiredBoolean(candidate.required, `application setup field ${index} required`),
      ...(candidate.placeholder === undefined ? {} : {
        placeholder: applicationPresentationText(candidate.placeholder, 96, `application setup field ${index} placeholder`, file),
      }),
      ...(candidate.help === undefined ? {} : {
        help: applicationPresentationText(candidate.help, 160, `application setup field ${index} help`, file),
      }),
    };
  });
  const identities = fields.map((field) => `${field.interface}\0${field.pointer}`);
  if (new Set(identities).size !== identities.length) {
    throw new Error(`application manifest setup fields contain duplicates: ${file}`);
  }
  return { fields };
}

function resolveAuthoredApplicationSetupFields(authoredFields, interfaceId, declarations, label) {
  const resolved = [];
  const mappedNames = new Set();
  for (const presentation of authoredFields.filter((field) => stringValue(field?.interface) === interfaceId)) {
    const pointer = stringValue(presentation?.pointer);
    const matches = declarations.filter((declaration) => (
      arrayValue(declaration?.pointers).map(stringValue).includes(pointer)
    ));
    if (matches.length !== 1) {
      throw new Error(`${label} field '${pointer}' must resolve to exactly one declared environment field`);
    }
    const name = stringValue(matches[0]?.name);
    if (!name || mappedNames.has(name)) {
      throw new Error(`${label} maps more than one presentation field to the same environment field`);
    }
    mappedNames.add(name);
    resolved.push({ presentation, declaration: matches[0] });
  }
  return resolved;
}

function normalizeRuntimeLaunchEnvironment(value) {
  if (value === null || value === undefined) return null;
  if (!isPlainRecord(value)) throw new Error("runtime launch environment must be an object or null");
  const output = {};
  for (const [name, entry] of Object.entries(value)) {
    if (typeof entry !== "string") throw new Error(`runtime launch environment '${name}' is not a string`);
    output[name] = entry;
  }
  return output;
}
