import type { OctopusProviderInterface } from "../electron";
import { DEFAULT_PROJECT_COLOR, projectColorKey } from "../design-system/projectColors";
import {
  CAPABILITY_KINDS,
  PROJECT_COLORS,
  PROJECT_ICONS,
  SAFE_ID,
  validateWorkspaceSetupDraft,
  type WorkspaceSetupCapability,
  type WorkspaceSetupCapabilityKind,
  type WorkspaceSetupDraft,
  type WorkspaceSetupProject,
} from "./workspaceSetup";

export const SETUP_FILE_SCHEMA = "octopus.setup";
export const SETUP_FILE_VERSION = 1;
export const SETUP_FILE_MAX_BYTES = 256 * 1024;

export type SetupFileProject = {
  id: string;
  parent: string | null;
  name: string;
  goal: string;
  icon: string;
  color: string;
};

export type SetupFileCapability = {
  kind: WorkspaceSetupCapabilityKind;
  id: string;
  projects: string[];
};

export type SetupFileModel = {
  id: string;
  reasoning_effort?: string;
  service_tier?: string;
};

export type SetupFile = {
  schema: typeof SETUP_FILE_SCHEMA;
  schema_version: typeof SETUP_FILE_VERSION;
  space: { name: string; goal: string };
  projects: SetupFileProject[];
  capabilities: SetupFileCapability[];
  model?: SetupFileModel;
};

export type SetupFileCatalogItem = {
  id: string;
  kind: WorkspaceSetupCapabilityKind;
  name: string;
  included: boolean;
  projectSelectable: boolean;
};

export function parseSetupFile(source: string): SetupFile {
  if (new TextEncoder().encode(source).byteLength > SETUP_FILE_MAX_BYTES) {
    throw new Error("This setup file is larger than Octopus accepts.");
  }
  let value: unknown;
  try {
    value = JSON.parse(source);
  } catch {
    throw new Error("This file is not valid JSON.");
  }
  const root = exact(value, ["schema", "schema_version", "space", "projects", "capabilities", "model"], ["model"], "The setup file");
  if (root.schema !== SETUP_FILE_SCHEMA) throw new Error("This is not an Octopus setup file.");
  if (root.schema_version !== SETUP_FILE_VERSION) {
    throw new Error(`This setup file uses format version ${String(root.schema_version)}, which this Octopus cannot read.`);
  }
  const space = exact(root.space, ["name", "goal"], [], "The space");
  const projectRows = list(root.projects, "Projects", 24);
  const projects = projectRows.map((row, index) => parseProject(row, index));
  const projectIds = new Set<string>();
  for (const project of projects) {
    if (projectIds.has(project.id)) throw new Error(`The setup file names project '${project.id}' twice.`);
    projectIds.add(project.id);
  }
  for (const project of projects) {
    if (project.parent !== null && !projectIds.has(project.parent)) {
      throw new Error(`Project '${project.name}' is placed in '${project.parent}', which the file does not define.`);
    }
  }
  const capabilities = list(root.capabilities, "Capabilities", 128).map((row, index) => parseCapability(row, index, projectIds));
  const seen = new Set<string>();
  for (const capability of capabilities) {
    const identity = `${capability.kind}:${capability.id}`;
    if (seen.has(identity)) throw new Error(`The setup file lists '${capability.id}' twice.`);
    seen.add(identity);
  }
  return {
    schema: SETUP_FILE_SCHEMA,
    schema_version: SETUP_FILE_VERSION,
    space: { name: text(space.name, "The space name", 80), goal: text(space.goal, "The space goal", 240) },
    projects,
    capabilities,
    ...(root.model === undefined ? {} : { model: parseModel(root.model) }),
  };
}

export function serializeSetupFile(file: SetupFile): string {
  return `${JSON.stringify(file, null, 2)}\n`;
}

export function setupFileFromDraft(value: WorkspaceSetupDraft): SetupFile {
  const draft = validateWorkspaceSetupDraft(value);
  const fileIds = projectFileIds(draft.projects);
  const projects = orderedProjects(draft.projects).map((project) => ({
    id: required(fileIds.get(project.key)),
    parent: project.parentKey === null ? null : required(fileIds.get(project.parentKey)),
    name: project.name,
    goal: project.goal,
    icon: project.icon,
    color: project.color,
  }));
  const capabilities = draft.capabilities
    .filter((capability) => capability.selected || capability.included)
    .map((capability) => ({
      kind: capability.kind,
      id: capability.id,
      projects: capability.projectKeys.map((key) => required(fileIds.get(key))).sort(),
    }))
    .sort((left, right) => left.kind.localeCompare(right.kind) || left.id.localeCompare(right.id));
  return {
    schema: SETUP_FILE_SCHEMA,
    schema_version: SETUP_FILE_VERSION,
    space: { name: draft.spaceName, goal: draft.spaceGoal },
    projects,
    capabilities,
    ...(draft.providerModel === undefined ? {} : {
      model: {
        id: draft.providerModel,
        ...(draft.reasoningEffort ? { reasoning_effort: draft.reasoningEffort } : {}),
        ...(draft.serviceTier ? { service_tier: draft.serviceTier } : {}),
      },
    }),
  };
}

export function draftFromSetupFile(
  file: SetupFile,
  base: WorkspaceSetupDraft,
  catalog: readonly SetupFileCatalogItem[],
  provider: OctopusProviderInterface | null,
  idFactory: () => string = () => crypto.randomUUID(),
  timestamp = new Date().toISOString(),
): WorkspaceSetupDraft {
  const offered = new Map(catalog.map((item) => [`${item.kind}:${item.id}`, item]));
  const missing = file.capabilities.filter((capability) => !offered.has(`${capability.kind}:${capability.id}`));
  if (missing.length) {
    throw new Error(`This setup needs ${missing.map((capability) => `'${capability.id}'`).join(", ")}, which this Octopus does not offer.`);
  }
  for (const capability of file.capabilities) {
    const item = required(offered.get(`${capability.kind}:${capability.id}`));
    if (capability.projects.length && !item.projectSelectable) {
      throw new Error(`'${item.name}' cannot be limited to projects, but the setup file assigns it to some.`);
    }
  }
  const keys = new Map(file.projects.map((project) => [project.id, `project-${idFactory()}`]));
  const projects: WorkspaceSetupProject[] = file.projects.map((project) => ({
    key: required(keys.get(project.id)),
    parentKey: project.parent === null ? null : required(keys.get(project.parent)),
    name: project.name,
    goal: project.goal,
    icon: project.icon,
    color: project.color,
  }));
  const listed = new Map(file.capabilities.map((capability) => [`${capability.kind}:${capability.id}`, capability]));
  const capabilities: WorkspaceSetupCapability[] = catalog.map((item) => {
    const entry = listed.get(`${item.kind}:${item.id}`);
    const selected = item.included || entry !== undefined;
    return {
      id: item.id,
      kind: item.kind,
      included: item.included,
      selected,
      projectKeys: item.projectSelectable && selected
        ? entry
          ? entry.projects.map((projectId) => required(keys.get(projectId)))
          : projects.map((project) => project.key)
        : [],
    };
  });
  const model = file.model ? chosenModel(file.model, provider) : {
    ...(base.providerModel === undefined ? {} : { providerModel: base.providerModel }),
    ...(base.reasoningEffort === undefined ? {} : { reasoningEffort: base.reasoningEffort }),
    ...(base.serviceTier === undefined ? {} : { serviceTier: base.serviceTier }),
  };
  return validateWorkspaceSetupDraft({
    schemaVersion: 1,
    operationId: base.operationId,
    preset: "custom",
    step: "space",
    spaceName: file.space.name,
    spaceGoal: file.space.goal,
    projects,
    capabilities,
    updatedAt: timestamp,
    ...model,
  });
}

export type HomeSetupSources = {
  scope: unknown;
  localPackages: unknown;
  applications: unknown;
  drivers: unknown;
  installedExtensionIds: ReadonlySet<string>;
};

export function setupFileFromHome(sources: HomeSetupSources): SetupFile {
  const scope = record(sources.scope, "This Home's scope");
  const rootEeId = typeof scope.root_ee_id === "string" ? scope.root_ee_id : "";
  if (!rootEeId || scope.status !== "ready") throw new Error("This Home is not set up yet, so there is no setup to export.");
  const root = record(scope.root_environment, "This Home's space");
  const environments = rows(scope.projects).filter((row) => row.ee_id !== rootEeId);
  const presentations = new Map(rows(scope.project_presentations).map((row) => [String(row.ee_id), row]));
  const byEe = new Map(environments.map((row) => [String(row.ee_id), row]));
  const draftProjects: WorkspaceSetupProject[] = environments.map((row) => {
    const eeId = String(row.ee_id);
    const parent = String(row.parent_ee_id ?? "");
    if (parent !== rootEeId && !byEe.has(parent)) throw new Error(`Project '${String(row.name || eeId)}' sits outside this Home's space.`);
    const presentation = presentations.get(eeId) ?? {};
    const name = String(presentation.name || row.name || eeId).trim();
    const icon = String(presentation.icon ?? "");
    return {
      key: eeId,
      parentKey: parent === rootEeId ? null : parent,
      name,
      goal: String(row.goal || name).trim(),
      icon: PROJECT_ICONS.has(icon) ? icon : "briefcase",
      color: projectColorKey(String(presentation.color ?? "")) ?? DEFAULT_PROJECT_COLOR,
    };
  });
  const fileIds = projectFileIds(draftProjects);
  const packs = rows(record(sources.localPackages, "Local packages").packs).filter((pack) => pack.workspace_setup !== undefined);
  if (packs.length !== 1) throw new Error("This Home's setup pack could not be identified.");
  const pack = packs[0];
  const setup = record(pack.workspace_setup, "The setup pack");
  const projectSelectable = new Set([
    ...rows(setup.project_defaults).map((row) => String(row.package_id)),
    ...rows(setup.agent_presets).map((row) => String(row.component_package_id)),
  ]);
  const states = new Map<string, Record<string, unknown>[]>();
  for (const row of [
    ...rows(record(sources.applications, "Applications").applications),
    ...rows(record(sources.drivers, "Drivers").drivers),
  ]) states.set(String(row.package_id), rows(row.project_states));
  const capabilities: SetupFileCapability[] = [{ kind: "pack", id: String(pack.id), projects: [] }];
  for (const component of rows(pack.components)) {
    const packageId = String(component.package_id);
    if (component.selection !== "recommended" && !projectSelectable.has(packageId)) continue;
    if (component.installed !== true || component.receipt_state !== "valid") continue;
    const kind = String(component.product_kind) as WorkspaceSetupCapabilityKind;
    const projects = projectSelectable.has(packageId)
      ? (states.get(packageId) ?? [])
        .filter((state) => state.is_root !== true && state.policy_allowed === true && fileIds.has(String(state.ee_id)))
        .map((state) => required(fileIds.get(String(state.ee_id))))
        .sort()
      : [];
    capabilities.push({ kind, id: packageId, projects });
  }
  for (const extension of rows(pack.extensions)) {
    const extensionId = String(extension.extension_id);
    if (sources.installedExtensionIds.has(extensionId)) capabilities.push({ kind: "extension", id: extensionId, projects: [] });
  }
  const file: SetupFile = {
    schema: SETUP_FILE_SCHEMA,
    schema_version: SETUP_FILE_VERSION,
    space: {
      name: String(root.name || scope.space_name || "My space").trim(),
      goal: String(root.goal || root.name || "My space").trim(),
    },
    projects: orderedProjects(draftProjects).map((project) => ({
      id: required(fileIds.get(project.key)),
      parent: project.parentKey === null ? null : required(fileIds.get(project.parentKey)),
      name: project.name,
      goal: project.goal,
      icon: project.icon,
      color: project.color,
    })),
    capabilities: capabilities.sort((left, right) => left.kind.localeCompare(right.kind) || left.id.localeCompare(right.id)),
  };
  return parseSetupFile(serializeSetupFile(file));
}

export function setupFileName(file: SetupFile): string {
  const slug = slugify(file.space.name);
  return `${slug || "octopus"}.octopus-setup.json`;
}

function chosenModel(
  model: SetupFileModel,
  provider: OctopusProviderInterface | null,
): Pick<WorkspaceSetupDraft, "providerModel" | "reasoningEffort" | "serviceTier"> {
  const offered = provider?.models.find((candidate) => candidate.model === model.id && candidate.showInPicker);
  if (!offered) throw new Error(`This setup chooses the model '${model.id}', which is not available here.`);
  if (model.reasoning_effort && offered.supportedReasoningEfforts.length && !offered.supportedReasoningEfforts.includes(model.reasoning_effort)) {
    throw new Error(`The model '${offered.displayName || offered.model}' does not offer the reasoning effort '${model.reasoning_effort}'.`);
  }
  const tier = model.service_tier ?? offered.defaultServiceTier ?? "default";
  if (tier !== "default" && !(offered.serviceTiers ?? []).some((candidate) => candidate.id === tier)) {
    throw new Error(`The model '${offered.displayName || offered.model}' does not offer the speed '${tier}'.`);
  }
  return {
    providerModel: offered.model,
    reasoningEffort: model.reasoning_effort ?? offered.defaultReasoningEffort ?? "",
    serviceTier: tier,
  };
}

function parseProject(value: unknown, index: number): SetupFileProject {
  const label = `Project ${index + 1}`;
  const row = exact(value, ["id", "parent", "name", "goal", "icon", "color"], [], label);
  const name = text(row.name, `${label} name`, 80);
  const icon = text(row.icon, `Project '${name}' icon`, 40);
  const color = text(row.color, `Project '${name}' color`, 40);
  if (!PROJECT_ICONS.has(icon)) throw new Error(`Project '${name}' uses the icon '${icon}', which Octopus does not have.`);
  if (!PROJECT_COLORS.has(color)) throw new Error(`Project '${name}' uses the color '${color}', which Octopus does not have.`);
  return {
    id: identifier(row.id, `${label} id`),
    parent: row.parent === null ? null : identifier(row.parent, `Project '${name}' parent`),
    name,
    goal: text(row.goal, `Project '${name}' goal`, 240),
    icon,
    color,
  };
}

function parseCapability(value: unknown, index: number, projectIds: ReadonlySet<string>): SetupFileCapability {
  const label = `Capability ${index + 1}`;
  const row = exact(value, ["kind", "id", "projects"], [], label);
  const id = identifier(row.id, `${label} id`);
  const kind = text(row.kind, `'${id}' kind`, 40) as WorkspaceSetupCapabilityKind;
  if (!CAPABILITY_KINDS.has(kind)) throw new Error(`'${id}' has the kind '${kind}', which a setup cannot carry.`);
  const projects = list(row.projects, `'${id}' projects`, 24).map((projectId) => identifier(projectId, `'${id}' project`));
  for (const projectId of projects) {
    if (!projectIds.has(projectId)) throw new Error(`'${id}' is assigned to project '${projectId}', which the file does not define.`);
  }
  if (new Set(projects).size !== projects.length) throw new Error(`'${id}' names a project twice.`);
  return { kind, id, projects: [...projects].sort() };
}

function parseModel(value: unknown): SetupFileModel {
  const row = exact(value, ["id", "reasoning_effort", "service_tier"], ["reasoning_effort", "service_tier"], "The model");
  return {
    id: text(row.id, "The model id", 256),
    ...(row.reasoning_effort === undefined ? {} : { reasoning_effort: text(row.reasoning_effort, "The reasoning effort", 80) }),
    ...(row.service_tier === undefined ? {} : { service_tier: text(row.service_tier, "The model speed", 80) }),
  };
}

function orderedProjects(projects: readonly WorkspaceSetupProject[]): WorkspaceSetupProject[] {
  const placed = new Set<string>();
  const ordered: WorkspaceSetupProject[] = [];
  let pending = [...projects];
  while (pending.length) {
    const ready = pending.filter((project) => project.parentKey === null || placed.has(project.parentKey));
    if (!ready.length) throw new Error("The project tree cannot be ordered.");
    for (const project of ready) {
      placed.add(project.key);
      ordered.push(project);
    }
    pending = pending.filter((project) => !placed.has(project.key));
  }
  return ordered;
}

function projectFileIds(projects: readonly WorkspaceSetupProject[]): Map<string, string> {
  const used = new Set<string>();
  const ids = new Map<string, string>();
  for (const project of orderedProjects(projects)) {
    const base = slugify(project.name) || "project";
    let candidate = base;
    for (let suffix = 2; used.has(candidate); suffix += 1) candidate = `${base}-${suffix}`;
    used.add(candidate);
    ids.set(project.key, candidate);
  }
  return ids;
}

function slugify(value: string): string {
  return value
    .normalize("NFKD")
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, 60);
}

function record(value: unknown, label: string): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error(`${label} could not be read.`);
  return value as Record<string, unknown>;
}

function rows(value: unknown): Record<string, unknown>[] {
  return Array.isArray(value)
    ? value.filter((row): row is Record<string, unknown> => Boolean(row) && typeof row === "object" && !Array.isArray(row))
    : [];
}

function exact(value: unknown, keys: readonly string[], optional: readonly string[], label: string): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error(`${label} must be an object.`);
  const row = value as Record<string, unknown>;
  const unknown = Object.keys(row).filter((key) => !keys.includes(key));
  if (unknown.length) throw new Error(`${label} has fields Octopus does not read: ${unknown.join(", ")}.`);
  const absent = keys.filter((key) => !optional.includes(key) && !(key in row));
  if (absent.length) throw new Error(`${label} is missing ${absent.join(", ")}.`);
  return row;
}

function list(value: unknown, label: string, limit: number): unknown[] {
  if (!Array.isArray(value)) throw new Error(`${label} must be a list.`);
  if (value.length > limit) throw new Error(`${label} can hold at most ${limit} entries.`);
  return value;
}

function text(value: unknown, label: string, limit: number): string {
  if (typeof value !== "string" || !value.trim()) throw new Error(`${label} must be non-empty text.`);
  const trimmed = value.trim();
  if (trimmed.length > limit) throw new Error(`${label} is longer than ${limit} characters.`);
  if (/[\u0000-\u001f\u007f]/.test(trimmed)) throw new Error(`${label} contains control characters.`);
  return trimmed;
}

function identifier(value: unknown, label: string): string {
  if (typeof value !== "string" || !SAFE_ID.test(value)) throw new Error(`${label} must be a short id of letters, digits, dots, dashes or underscores.`);
  return value;
}

function required<T>(value: T | undefined): T {
  if (value === undefined) throw new Error("The setup refers to something it does not define.");
  return value;
}
