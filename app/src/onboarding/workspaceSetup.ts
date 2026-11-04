export const WORKSPACE_SETUP_STORAGE_KEY = "octopus.workspace-setup.v1";

export type WorkspaceSetupPreset = "life" | "work" | "founder" | "custom";
export type WorkspaceSetupStep = "space" | "projects" | "capabilities" | "review" | "applying";

export type WorkspaceSetupProject = {
  key: string;
  parentKey: string | null;
  name: string;
  goal: string;
  icon: string;
  color: string;
};

export type WorkspaceSetupCapabilityKind = "application" | "driver" | "artifact" | "extension" | "pack";

export type WorkspaceSetupCapability = {
  id: string;
  kind: WorkspaceSetupCapabilityKind;
  selected: boolean;
  included: boolean;
  projectKeys: string[];
};

export type WorkspaceSetupDraft = {
  providerModel?: string;
  reasoningEffort?: string;
  serviceTier?: string;
  schemaVersion: 1;
  operationId: string;
  preset: WorkspaceSetupPreset;
  step: WorkspaceSetupStep;
  spaceName: string;
  spaceGoal: string;
  projects: WorkspaceSetupProject[];
  capabilities: WorkspaceSetupCapability[];
  updatedAt: string;
};

export type WorkspaceSetupIntent = {
  operationId: string;
  root: {
    key: "root";
    templateName: "existence";
    name: string;
    goal: string;
  };
  home: {
    name: "Home";
  };
  projects: Array<{
    key: string;
    parentKey: "root" | string;
    templateName: "project";
    name: string;
    goal: string;
    presentation: {
      icon: string;
      color: string;
    };
  }>;
  capabilities: Array<{
    id: string;
    kind: WorkspaceSetupCapabilityKind;
    included: boolean;
    projectKeys: string[];
  }>;
};

export type WorkspaceSetupHomeRecipe = {
  agent_presets?: WorkspaceSetupAgentPreset[];
  task: WorkspaceSetupTaskRecipe;
  provider_interface_id: string;
  worker_interface_id: string;
  door_host_slots?: Record<string, Record<string, string>>;
  context_artifact_bindings: Array<{ id: string }>;
  selected_worker_environment_kinds: string[];
  project_defaults: Array<{
    package_id: string;
    default_for_new_tasks: boolean;
  }>;
};

export type WorkspaceSetupTaskRecipe = {
  worker_interface_id: string;
  context_artifact_bindings: Array<{ id: string }>;
  selected_worker_environment_kinds: string[];
};

export type WorkspaceSetupAgentPreset = {
  id: string;
  component_package_id: string;
  name: string;
  icon: string;
  worker_interface_id: string;
  provider_interface_id: string;
  context_artifact_bindings: Array<{ id: string }>;
  selected_worker_environment_kinds: string[];
  selected_application_ids: string[];
};

export type WorkspaceAgentPresetChoice = {
  packId: string;
  expectedPackManifestDigest: string;
  presetId: string;
  projectKeys: string[];
  providerInterfaceId: string;
  providerModel: string;
  reasoningEffort: string;
  serviceTier?: string;
};

export type WorkspaceInitializeRequest = {
  agentPresets?: WorkspaceAgentPresetChoice[];
  action: "initialize_workspace";
  operationId: string;
  root: WorkspaceSetupIntent["root"] | {
    key: "root";
    existingEeId: string;
    expectedEeDigest: string;
    name: string;
  };
  home: {
    name: "Home";
    providerInterfaceId: string;
    providerModel: string;
    reasoningEffort: string;
  serviceTier?: string;
    workerInterfaceId: string;
    doorHostSlots: Record<string, Record<string, string>>;
    contextArtifactBindings: Array<{ id: string }>;
    selectedWorkerEnvironmentKinds: string[];
  };
  task: {
    workerInterfaceId: string;
    contextArtifactBindings: Array<{ id: string }>;
    selectedWorkerEnvironmentKinds: string[];
  };
  projects: WorkspaceSetupIntent["projects"];
};

type PresetDefinition = {
  spaceName: string;
  spaceGoal: string;
  projects: Array<Omit<WorkspaceSetupProject, "key"> & { seed: string }>;
};

const PRESET_DEFINITIONS: Record<WorkspaceSetupPreset, PresetDefinition> = {
  life: {
    spaceName: "Life",
    spaceGoal: "Organize personal life",
    projects: [
      projectSeed("study", null, "Study", "book", "blue"),
      projectSeed("travel", null, "Travel", "globe", "cyan"),
    ],
  },
  work: {
    spaceName: "Work",
    spaceGoal: "Operate work",
    projects: [
      projectSeed("work-project-1", null, "Project 1", "briefcase", "indigo"),
      projectSeed("work-project-2", null, "Project 2", "folder", "violet"),
    ],
  },
  founder: {
    spaceName: "Founder",
    spaceGoal: "Operate life and work",
    projects: [
      projectSeed("life", null, "Life", "globe", "teal"),
      projectSeed("work", null, "Work", "briefcase", "indigo"),
      projectSeed("founder-project-1", "work", "Project 1", "rocket", "violet"),
      projectSeed("founder-project-2", "work", "Project 2", "code", "blue"),
      projectSeed("founder-project-3", "work", "Project 3", "storefront", "amber"),
    ],
  },
  custom: {
    spaceName: "My space",
    spaceGoal: "Organize my work and life",
    projects: [
      projectSeed("custom-project-1", null, "Project", "folder", "violet"),
    ],
  },
};

export const CAPABILITY_KINDS: ReadonlySet<WorkspaceSetupCapabilityKind> = new Set<WorkspaceSetupCapabilityKind>([
  "application",
  "driver",
  "artifact",
  "extension",
  "pack",
]);

export const SAFE_ID = /^[A-Za-z0-9][A-Za-z0-9._-]{0,159}$/;
const SAFE_OPERATION_ID = /^[A-Za-z0-9_-]{8,96}$/;
export const PROJECT_ICONS: ReadonlySet<string> = new Set(["briefcase", "folder", "code", "globe", "rocket", "palette", "storefront", "book"]);
export const PROJECT_COLORS: ReadonlySet<string> = new Set(["black", "violet", "indigo", "blue", "cyan", "teal", "green", "amber", "orange", "pink", "red"]);

export function createWorkspaceSetupDraft(
  preset: WorkspaceSetupPreset,
  idFactory: () => string = () => crypto.randomUUID(),
  timestamp = new Date().toISOString(),
): WorkspaceSetupDraft {
  const definition = PRESET_DEFINITIONS[preset];
  const projectKeys = new Map(definition.projects.map((project) => [project.seed, `project-${idFactory()}`]));
  const projects = definition.projects.map(({ seed, ...project }) => ({
    ...project,
    key: requiredMapValue(projectKeys, seed),
    parentKey: project.parentKey ? requiredMapValue(projectKeys, project.parentKey) : null,
  }));
  return validateWorkspaceSetupDraft({
    schemaVersion: 1,
    operationId: idFactory(),
    preset,
    step: "space",
    spaceName: definition.spaceName,
    spaceGoal: definition.spaceGoal,
    projects,
    capabilities: [],
    updatedAt: timestamp,
  });
}

export function replaceWorkspaceSetupPreset(
  draft: WorkspaceSetupDraft,
  preset: WorkspaceSetupPreset,
  idFactory: () => string = () => crypto.randomUUID(),
  timestamp = new Date().toISOString(),
): WorkspaceSetupDraft {
  if (draft.preset === preset) return draft;
  const next = createWorkspaceSetupDraft(preset, idFactory, timestamp);
  return validateWorkspaceSetupDraft({
    ...next,
    operationId: draft.operationId,
    ...(draft.providerModel !== undefined ? { providerModel: draft.providerModel } : {}),
    ...(draft.serviceTier !== undefined ? { serviceTier: draft.serviceTier } : {}),
    ...(draft.reasoningEffort !== undefined ? { reasoningEffort: draft.reasoningEffort } : {}),
    capabilities: draft.capabilities.map((capability) => ({ ...capability, projectKeys: [] })),
  });
}

export function validateWorkspaceSetupDraft(value: unknown): WorkspaceSetupDraft {
  const record = exactRecord(
    value,
    ["schemaVersion", "operationId", "preset", "step", "spaceName", "spaceGoal", "projects", "capabilities", "updatedAt", "providerModel", "reasoningEffort", "serviceTier"],
    "Workspace setup draft",
    ["providerModel", "reasoningEffort", "serviceTier"],
  );
  if (record.schemaVersion !== 1) throw new Error("Workspace setup draft has an unsupported schema version");
  const preset = requiredPreset(record.preset);
  const step = requiredStep(record.step);
  const operationId = requiredOperationId(record.operationId);
  const spaceName = requiredText(record.spaceName, "Workspace name", 80);
  const spaceGoal = requiredText(record.spaceGoal, "Workspace goal", 240);
  const updatedAt = requiredTimestamp(record.updatedAt, "Workspace setup update time");
  if (!Array.isArray(record.projects)) throw new Error("Workspace setup projects must be an array");
  if (record.projects.length > 24) throw new Error("Workspace setup supports at most 24 projects during Welcome");
  const projects = record.projects.map((project, index) => parseProject(project, index));
  validateProjectTree(projects);
  if (!Array.isArray(record.capabilities)) throw new Error("Workspace setup capabilities must be an array");
  const capabilities = record.capabilities.map((capability, index) => parseCapability(capability, index));
  validateCapabilities(capabilities, new Set(projects.map((project) => project.key)));
  if (record.serviceTier !== undefined && (typeof record.serviceTier !== "string" || record.serviceTier.trim() !== record.serviceTier || record.serviceTier.length > 80 || /[\u0000-\u001f\u007f]/.test(record.serviceTier))) throw new Error("Invalid service tier");
  if (record.reasoningEffort !== undefined && (typeof record.reasoningEffort !== "string" || record.reasoningEffort.length > 80)) throw new Error("Invalid reasoning effort");
  return {
    schemaVersion: 1,
    operationId,
    preset,
    step,
    spaceName,
    spaceGoal,
    projects,
    capabilities,
    updatedAt,
    ...(record.providerModel !== undefined ? { providerModel: requiredText(record.providerModel, "Selected model", 256) } : {}),
    ...(record.serviceTier !== undefined ? { serviceTier: record.serviceTier as string } : {}),
    ...(record.reasoningEffort !== undefined ? { reasoningEffort: record.reasoningEffort as string } : {}),
  };
}

export function compileWorkspaceSetupDraft(value: WorkspaceSetupDraft): WorkspaceSetupIntent {
  const draft = validateWorkspaceSetupDraft(value);
  const orderedProjects = topologicalProjects(draft.projects);
  return {
    operationId: draft.operationId,
    root: {
      key: "root",
      templateName: "existence",
      name: draft.spaceName,
      goal: draft.spaceGoal,
    },
    home: { name: "Home" },
    projects: orderedProjects.map((project) => ({
      key: project.key,
      parentKey: project.parentKey ?? "root",
      templateName: "project",
      name: project.name,
      goal: project.goal,
      presentation: {
        icon: project.icon,
        color: project.color,
      },
    })),
    capabilities: draft.capabilities
      .filter((capability) => capability.selected || capability.included)
      .map((capability) => ({
        id: capability.id,
        kind: capability.kind,
        included: capability.included,
        projectKeys: [...capability.projectKeys].sort(),
      }))
      .sort((left, right) => left.kind.localeCompare(right.kind) || left.id.localeCompare(right.id)),
  };
}

export function compileWorkspaceInitializeRequest(
  value: WorkspaceSetupDraft,
  recipeValue: WorkspaceSetupHomeRecipe,
  pack?: { id: string; manifestDigest: string },
): WorkspaceInitializeRequest {
  const intent = compileWorkspaceSetupDraft(value);
  const recipe = exactRecord(
    recipeValue,
    [
      "provider_interface_id",
      "worker_interface_id",
      "door_host_slots",
      "context_artifact_bindings",
      "selected_worker_environment_kinds",
      "project_defaults",
      "agent_presets",
      "task",
    ],
    "Workspace Home recipe",
    ["agent_presets", "door_host_slots"],
  );
  if (!Array.isArray(recipe.context_artifact_bindings)) {
    throw new Error("Workspace Home recipe context bindings must be an array");
  }
  if (!Array.isArray(recipe.selected_worker_environment_kinds)) {
    throw new Error("Workspace Home recipe worker environments must be an array");
  }
  if (!Array.isArray(recipe.project_defaults)) {
    throw new Error("Workspace Home recipe Project defaults must be an array");
  }
  if (recipe.agent_presets !== undefined && !Array.isArray(recipe.agent_presets)) {
    throw new Error("Workspace Home recipe Agent presets must be an array");
  }
  const contextArtifactBindings = recipe.context_artifact_bindings.map((binding, index) => {
    const row = exactRecord(binding, ["id"], `Workspace Home context binding ${index + 1}`);
    return { id: requiredId(row.id, `Workspace Home context binding ${index + 1}`) };
  });
  const selectedWorkerEnvironmentKinds = recipe.selected_worker_environment_kinds.map((kind, index) => (
    requiredId(kind, `Workspace Home worker environment ${index + 1}`)
  ));
  const taskRecipe = exactRecord(recipe.task, ["worker_interface_id", "context_artifact_bindings", "selected_worker_environment_kinds"], "Workspace Task recipe");
  if (!Array.isArray(taskRecipe.context_artifact_bindings) || !Array.isArray(taskRecipe.selected_worker_environment_kinds)) {
    throw new Error("Workspace Task recipe requires context and worker environment arrays");
  }
  const task = {
    workerInterfaceId: requiredId(taskRecipe.worker_interface_id, "Workspace Task worker"),
    contextArtifactBindings: taskRecipe.context_artifact_bindings.map((value, index) => {
      const binding = exactRecord(value, ["id"], `Workspace Task context ${index + 1}`);
      return { id: requiredId(binding.id, `Workspace Task context ${index + 1}`) };
    }),
    selectedWorkerEnvironmentKinds: taskRecipe.selected_worker_environment_kinds.map((value, index) => requiredId(value, `Workspace Task environment ${index + 1}`)),
  };
  if (new Set(task.contextArtifactBindings.map((binding) => binding.id)).size !== task.contextArtifactBindings.length
    || new Set(task.selectedWorkerEnvironmentKinds).size !== task.selectedWorkerEnvironmentKinds.length) {
    throw new Error("Workspace Task recipe repeats a binding");
  }
  const projectDefaults = recipe.project_defaults.map((value, index) => {
    const row = exactRecord(
      value,
      ["package_id", "default_for_new_tasks"],
      `Workspace Home Project default ${index + 1}`,
    );
    if (typeof row.default_for_new_tasks !== "boolean") {
      throw new Error(`Workspace Home Project default ${index + 1} has an invalid selection`);
    }
    return requiredId(row.package_id, `Workspace Home Project default ${index + 1} package`);
  });
  if (
    new Set(contextArtifactBindings.map((binding) => binding.id)).size !== contextArtifactBindings.length
    || new Set(selectedWorkerEnvironmentKinds).size !== selectedWorkerEnvironmentKinds.length
    || new Set(projectDefaults).size !== projectDefaults.length
  ) {
    throw new Error("Workspace Home recipe repeats a binding");
  }
  const agentPresets = (recipeValue.agent_presets ?? []).flatMap((preset) => {
    const capability = value.capabilities.find((entry) => entry.id === preset.component_package_id && entry.selected);
    if (!capability?.projectKeys.length) return [];
    if (!pack || !/^[a-f0-9]{64}$/.test(pack.manifestDigest)) throw new Error("Agent presets require an exact pack snapshot");
    return [{ packId: pack.id, expectedPackManifestDigest: pack.manifestDigest, presetId: preset.id, projectKeys: [...capability.projectKeys].sort(), providerInterfaceId: preset.provider_interface_id, providerModel: requiredText(value.providerModel, "Selected model", 256), serviceTier: value.serviceTier ?? "default", reasoningEffort: value.reasoningEffort ?? "" }];
  }).sort((a, b) => `${a.packId}\0${a.presetId}`.localeCompare(`${b.packId}\0${b.presetId}`));
  return {
    action: "initialize_workspace",
    operationId: intent.operationId,
    root: intent.root,
    home: {
      name: "Home",
      providerInterfaceId: requiredId(recipe.provider_interface_id, "Workspace Home provider"),
      providerModel: requiredText(value.providerModel, "Selected model", 256),
      serviceTier: value.serviceTier ?? "default", reasoningEffort: value.reasoningEffort ?? "",
      workerInterfaceId: requiredId(recipe.worker_interface_id, "Workspace Home worker"),
      doorHostSlots: homeDoorHostSlots(recipe.door_host_slots),
      contextArtifactBindings,
      selectedWorkerEnvironmentKinds,
    },
    task,
    projects: intent.projects,
    ...(agentPresets.length ? { agentPresets } : {}),
  };
}

export async function workspaceInitializeRequestDigest(request: WorkspaceInitializeRequest): Promise<string> {
  const canonical = {
    operationId: request.operationId,
    root: request.root,
    home: {
      ...request.home,
      reasoningEffort: request.home.reasoningEffort,
      serviceTier: request.home.serviceTier,
    },
    task: request.task,
    projects: request.projects,
    ...(request.agentPresets?.length ? { agentPresets: request.agentPresets } : {}),
  };
  const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(stableJson(canonical)));
  return [...new Uint8Array(digest)].map((byte) => byte.toString(16).padStart(2, "0")).join("");
}

export function readWorkspaceSetupDraft(): WorkspaceSetupDraft | null {
  const raw = window.localStorage.getItem(WORKSPACE_SETUP_STORAGE_KEY);
  if (raw === null) return null;
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch (error) {
    throw new Error(`Workspace setup draft is not valid JSON: ${error instanceof Error ? error.message : String(error)}`);
  }
  return validateWorkspaceSetupDraft(parsed);
}

export function saveWorkspaceSetupDraft(value: WorkspaceSetupDraft): WorkspaceSetupDraft {
  const draft = validateWorkspaceSetupDraft({ ...value, updatedAt: new Date().toISOString() });
  window.localStorage.setItem(WORKSPACE_SETUP_STORAGE_KEY, JSON.stringify(draft));
  return draft;
}

export function clearWorkspaceSetupDraft(): void {
  window.localStorage.removeItem(WORKSPACE_SETUP_STORAGE_KEY);
}

export function workspaceSetupTreeItems(projects: readonly WorkspaceSetupProject[]): Array<{
  id: string;
  label: string;
  parentId: string | null;
}> {
  const validated = validateWorkspaceSetupDraft({
    schemaVersion: 1,
    operationId: "preview_1",
    preset: "custom",
    step: "projects",
    spaceName: "Preview",
    spaceGoal: "Preview",
    projects,
    capabilities: [],
    updatedAt: new Date(0).toISOString(),
  });
  return topologicalProjects(validated.projects).map((project) => ({
    id: project.key,
    label: project.name,
    parentId: project.parentKey,
  }));
}

function projectSeed(
  seed: string,
  parentKey: string | null,
  name: string,
  icon: string,
  color: string,
): Omit<WorkspaceSetupProject, "key"> & { seed: string } {
  return { seed, parentKey, name, goal: name, icon, color };
}

function stableJson(value: unknown): string {
  if (Array.isArray(value)) return `[${value.map(stableJson).join(",")}]`;
  if (value && typeof value === "object") {
    const row = value as Record<string, unknown>;
    return `{${Object.keys(row).sort().map((key) => `${JSON.stringify(key)}:${stableJson(row[key])}`).join(",")}}`;
  }
  return JSON.stringify(value);
}

function parseProject(value: unknown, index: number): WorkspaceSetupProject {
  const project = exactRecord(
    value,
    ["key", "parentKey", "name", "goal", "icon", "color"],
    `Workspace project ${index + 1}`,
  );
  const key = requiredId(project.key, `Workspace project ${index + 1} key`);
  const parentKey = project.parentKey === null
    ? null
    : requiredId(project.parentKey, `Workspace project ${index + 1} parent`);
  const name = requiredText(project.name, `Workspace project ${index + 1} name`, 80);
  const goal = requiredText(project.goal, `Workspace project ${index + 1} goal`, 240);
  const icon = requiredText(project.icon, `Workspace project ${index + 1} icon`, 40);
  const color = requiredText(project.color, `Workspace project ${index + 1} color`, 40);
  if (!PROJECT_ICONS.has(icon)) throw new Error(`Workspace project '${name}' has an unsupported icon`);
  if (!PROJECT_COLORS.has(color)) throw new Error(`Workspace project '${name}' has an unsupported color`);
  return { key, parentKey, name, goal, icon, color };
}

function parseCapability(value: unknown, index: number): WorkspaceSetupCapability {
  const capability = exactRecord(
    value,
    ["id", "kind", "selected", "included", "projectKeys"],
    `Workspace capability ${index + 1}`,
  );
  const id = requiredId(capability.id, `Workspace capability ${index + 1} id`);
  const kind = requiredText(capability.kind, `Workspace capability ${index + 1} kind`, 40) as WorkspaceSetupCapabilityKind;
  if (!CAPABILITY_KINDS.has(kind)) throw new Error(`Workspace capability '${id}' has an unsupported kind`);
  if (typeof capability.selected !== "boolean" || typeof capability.included !== "boolean") {
    throw new Error(`Workspace capability '${id}' must declare selected and included`);
  }
  if (!Array.isArray(capability.projectKeys)) {
    throw new Error(`Workspace capability '${id}' has invalid project targets`);
  }
  const projectKeys = capability.projectKeys.map((key, projectIndex) => (
    requiredId(key, `Workspace capability '${id}' project ${projectIndex + 1}`)
  ));
  return {
    id,
    kind,
    selected: capability.selected,
    included: capability.included,
    projectKeys: [...new Set(projectKeys)],
  };
}

function validateProjectTree(projects: readonly WorkspaceSetupProject[]): void {
  const byKey = new Map<string, WorkspaceSetupProject>();
  for (const project of projects) {
    if (byKey.has(project.key)) throw new Error(`Workspace setup repeats project key '${project.key}'`);
    byKey.set(project.key, project);
  }
  for (const project of projects) {
    if (project.parentKey === project.key) throw new Error(`Workspace project '${project.name}' cannot contain itself`);
    if (project.parentKey && !byKey.has(project.parentKey)) {
      throw new Error(`Workspace project '${project.name}' references a missing parent`);
    }
    const seen = new Set([project.key]);
    let cursor = project.parentKey;
    let depth = 0;
    while (cursor) {
      if (seen.has(cursor)) throw new Error(`Workspace project '${project.name}' creates a cycle`);
      seen.add(cursor);
      cursor = byKey.get(cursor)?.parentKey ?? null;
      depth += 1;
      if (depth > 6) throw new Error("Workspace setup supports at most six nested project levels during Welcome");
    }
  }
}

function validateCapabilities(
  capabilities: readonly WorkspaceSetupCapability[],
  projectKeys: ReadonlySet<string>,
): void {
  const ids = new Set<string>();
  for (const capability of capabilities) {
    const identity = `${capability.kind}:${capability.id}`;
    if (ids.has(identity)) throw new Error(`Workspace setup repeats capability '${capability.id}'`);
    ids.add(identity);
    for (const projectKey of capability.projectKeys) {
      if (!projectKeys.has(projectKey)) {
        throw new Error(`Workspace capability '${capability.id}' targets a missing project`);
      }
    }
  }
}

function topologicalProjects(projects: readonly WorkspaceSetupProject[]): WorkspaceSetupProject[] {
  const remaining = new Map(projects.map((project) => [project.key, project]));
  const sourceOrder = new Map(projects.map((project, index) => [project.key, index]));
  const emitted = new Set<string>();
  const ordered: WorkspaceSetupProject[] = [];
  while (remaining.size) {
    const ready = [...remaining.values()]
      .filter((project) => !project.parentKey || emitted.has(project.parentKey))
      .sort((left, right) => (sourceOrder.get(left.key) ?? 0) - (sourceOrder.get(right.key) ?? 0));
    if (!ready.length) throw new Error("Workspace project tree cannot be ordered");
    for (const project of ready) {
      remaining.delete(project.key);
      emitted.add(project.key);
      ordered.push(project);
    }
  }
  return ordered;
}

function exactRecord(value: unknown, allowed: readonly string[], label: string, optional: readonly string[] = []): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error(`${label} must be an object`);
  const record = value as Record<string, unknown>;
  const unknown = Object.keys(record).filter((key) => !allowed.includes(key));
  if (unknown.length) throw new Error(`${label} contains unknown fields: ${unknown.join(", ")}`);
  const missing = allowed.filter((key) => !optional.includes(key) && !(key in record));
  if (missing.length) throw new Error(`${label} is missing fields: ${missing.join(", ")}`);
  return record;
}

function homeDoorHostSlots(value: unknown): Record<string, Record<string, string>> {
  if (value === undefined) return {};
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("Workspace Home folder access must be an object");
  return Object.fromEntries(Object.entries(value).map(([door, slots]) => {
    requiredId(door, "Workspace Home folder-access interface");
    if (!slots || typeof slots !== "object" || Array.isArray(slots)) throw new Error(`Workspace Home folder access '${door}' must be an object`);
    return [door, Object.fromEntries(Object.entries(slots).map(([slot, root]) => {
      requiredId(slot, "Workspace Home folder-access slot");
      if (typeof root !== "string" || !root.startsWith("/") || root.includes("\0") || root.trim() !== root) {
        throw new Error(`Workspace Home folder access '${door}.${slot}' requires an absolute path`);
      }
      return [slot, root];
    }))];
  }));
}

function requiredOperationId(value: unknown): string {
  if (typeof value !== "string" || !SAFE_OPERATION_ID.test(value)) {
    throw new Error("Workspace setup operation is invalid");
  }
  return value;
}

function requiredId(value: unknown, label: string): string {
  if (typeof value !== "string" || !SAFE_ID.test(value)) throw new Error(`${label} is invalid`);
  return value;
}

function requiredPreset(value: unknown): WorkspaceSetupPreset {
  if (value === "life" || value === "work" || value === "founder" || value === "custom") return value;
  throw new Error("Workspace setup preset is invalid");
}

function requiredStep(value: unknown): WorkspaceSetupStep {
  if (value === "space" || value === "projects" || value === "capabilities" || value === "review" || value === "applying") return value;
  throw new Error("Workspace setup step is invalid");
}

function requiredText(value: unknown, label: string, maxLength: number): string {
  if (typeof value !== "string") throw new Error(`${label} must be text`);
  const normalized = value.trim();
  if (!normalized) throw new Error(`${label} cannot be empty`);
  if (normalized.length > maxLength) throw new Error(`${label} exceeds ${maxLength} characters`);
  return normalized;
}

function requiredTimestamp(value: unknown, label: string): string {
  const timestamp = requiredText(value, label, 80);
  if (!Number.isFinite(Date.parse(timestamp))) throw new Error(`${label} is invalid`);
  return timestamp;
}

function requiredMapValue(values: ReadonlyMap<string, string>, key: string): string {
  const value = values.get(key);
  if (!value) throw new Error(`Workspace preset references missing project seed '${key}'`);
  return value;
}
