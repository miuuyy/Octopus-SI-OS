import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { createLatestProjectionRead } from "./latestProjectionRead";
import type { OctopusClientAccessProfile } from "../electron";
import type { ClientAgentPresetInput } from "../agentPresets";
import type { ProjectionResourcePhase } from "../components/projectionResource";
import { DEFAULT_PROJECT_COLOR } from "../design-system/projectColors";
import { useProjectionStructureGeneration } from "../projectionLive";
import { mutateOctopusOsWorkspace, readProjection } from "../projectionClient";
import { projectPolicyOption, type ProjectPolicyOption } from "./projectPolicyOptions";
import {
  readClientChatProjects,
  saveClientChatProjects,
  type ClientChatProject,
} from "./clientWorkspaces";

export type OctopusOsEeTemplateOption = ProjectPolicyOption;

export type OctopusOsEnvironmentSummary = {
  eeId: string;
  displayName: string;
  templateName: string;
};

export type OctopusOsProjectDraft = {
  sourceFolders: string[];
  name: string;
  icon: string;
  color: string;
  parentId: string;
  templateName: string;
};

export type OctopusOsProjectEdit = {
  sourceFolders: string[];
  projectId: string;
  name: string;
  icon: string;
  color: string;
};

export type OctopusOsProjectDeletionReceipt = {
  index: number;
  project: ClientChatProject;
  reparentedChildIds: string[];
};

export type OctopusOsProjectWorkspace = {
  projects: ClientChatProject[];
  rootEeId: string;
  homeRunId: string;
  homeInterfaceId: string;
  rootContext: OctopusOsEnvironmentSummary | null;
  rootAllowedChildTemplateIds: string[];
  templates: OctopusOsEeTemplateOption[];
  status: string;
  phase: ProjectionResourcePhase;
  pending: boolean;
  error: string;
  updatePresentation: (projects: ClientChatProject[]) => void;
  createProject: (draft: OctopusOsProjectDraft) => Promise<void>;
  createAgentTemplate: (input: ClientAgentPresetInput) => Promise<{ agentId: string; workerInterfaceId: string }>;
  configureAgentTemplate: (input: ClientAgentPresetInput, agentId: string, expectedClientTemplateDigest: string) => Promise<void>;
  deleteAgentTemplate: (agentId: string, projectEeId: string, workerInterfaceId: string, expectedClientTemplateDigest: string) => Promise<void>;
  editProject: (edit: OctopusOsProjectEdit) => Promise<void>;
  deleteProject: (projectId: string) => Promise<OctopusOsProjectDeletionReceipt | null>;
  restoreProject: (receipt: OctopusOsProjectDeletionReceipt) => Promise<void>;
  deleteTask: (runId: string) => Promise<void>;
  setTaskArchived: (runId: string, archived: boolean) => Promise<void>;
};

type ScopeEnvironment = {
  sourceFolders: string[];
  eeId: string;
  name: string;
  parentEeId: string;
  templateName: string;
  goal: string;
  status: string;
  recordDigest: string;
  allowedChildTemplateIds: string[];
  clientRunIds: string[];
  readOnlyRunIds: string[];
};

type ScopeSnapshot = {
  status: string;
  rootEeId: string;
  spaceName: string;
  homeRunId: string;
  homeInterfaceId: string;
  homeSidecarDigest: string;
  agentCatalogDigest: string;
  rootEnvironment: ScopeEnvironment | null;
  projects: ScopeEnvironment[];
  projectPresentations: ProjectPresentation[];
  templates: OctopusOsEeTemplateOption[];
  taskDigests: Map<string, string>;
};

type ProjectPresentation = {
  eeId: string;
  name: string;
  icon: string;
  color: string;
};

const EMPTY_SCOPE: ScopeSnapshot = {
  status: "unbound",
  rootEeId: "",
  spaceName: "",
  homeRunId: "",
  homeInterfaceId: "",
  homeSidecarDigest: "",
  agentCatalogDigest: "",
  rootEnvironment: null,
  projects: [],
  projectPresentations: [],
  templates: [],
  taskDigests: new Map(),
};

export function useOctopusOsProjects(profile: OctopusClientAccessProfile, runtimeHome: string): OctopusOsProjectWorkspace {
  const [projects, setProjects] = useState<ClientChatProject[]>(() => readClientChatProjects(profile));
  const [scope, setScope] = useState<ScopeSnapshot>(EMPTY_SCOPE);
  const [loaded, setLoaded] = useState(profile !== "live");
  const [reading, setReading] = useState(profile === "live");
  const [pending, setPending] = useState(false);
  const [error, setError] = useState("");
  const [readError, setReadError] = useState("");
  const requestGeneration = useRef(0);
  const scopeReads = useMemo(() => createLatestProjectionRead<unknown>(), [profile, runtimeHome]);
  useLayoutEffect(() => () => scopeReads.invalidate(), [scopeReads]);
  useLayoutEffect(() => {
    requestGeneration.current += 1;
    setScope(EMPTY_SCOPE);
    setLoaded(profile !== "live");
    setError("");
    setReadError("");
    setProjects(profile === "live" ? [] : readClientChatProjects(profile));
  }, [profile, runtimeHome]);
  const projectionGeneration = useProjectionStructureGeneration();
  const projectsRef = useRef(projects);
  projectsRef.current = projects;

  const reload = useCallback(async () => {
    const generation = requestGeneration.current + 1;
    requestGeneration.current = generation;
    setReading(true);
    try {
      const raw = await scopeReads.request(() => readProjection<unknown>("projection.operator.os.scope"));
      if (raw === undefined) return;
      if (profile !== "live") {
        const local = readClientChatProjects(profile);
        const projected = normalizeHomeProjection(raw);
        projectsRef.current = local;
        setProjects(local);
        setScope({ ...EMPTY_SCOPE, ...projected });
        const issue = projected.status === "invalid" ? "Octopus OS scope is invalid" : "";
        setLoaded(true);
        setReadError(issue);
        setError(issue);
        return;
      }
      const next = normalizeScope(raw);
      const merged = mergeProjectedProjects(next, projectPresentationRows(next));
      saveClientChatProjects(merged, profile);
      projectsRef.current = merged;
      setProjects(merged);
      setScope(next);
      const issue = next.status === "invalid" ? "Octopus OS scope is invalid" : "";
      setLoaded(true);
      setReadError(issue);
      setError(issue);
    } catch (failure) {
      if (generation !== requestGeneration.current) return;
      const message = failure instanceof Error ? failure.message : String(failure);
      setReadError(message);
      setError(message);
    } finally {
      if (generation === requestGeneration.current) setReading(false);
    }
  }, [profile, scopeReads]);

  useEffect(() => {
    void reload();
  }, [projectionGeneration, reload]);

  const perform = useCallback(async (operation: () => Promise<void>) => {
    if (profile !== "live") {
      await operation();
      return;
    }
    setPending(true);
    setError("");
    try {
      await operation();
      await reload();
    } catch (failure) {
      const message = failure instanceof Error ? failure.message : String(failure);
      await reload();
      setError(message);
      throw failure;
    } finally {
      setPending(false);
    }
  }, [profile, reload]);

  const persistPresentation = useCallback(async (next: ClientChatProject[]) => {
    saveClientChatProjects(next, profile);
    projectsRef.current = next;
    setProjects(next);
  }, [profile]);

  const updatePresentation = useCallback((next: ClientChatProject[]) => {
    const operation = profile === "live"
      ? perform(async () => {
          await mutateOctopusOsWorkspace({
            action: "update_project_presentation",
            operationId: crypto.randomUUID(),
            workspace: workspaceMutationIdentity(scope),
            projects: next.map((project) => ({
              eeId: project.id,
              icon: project.icon,
              color: project.color,
            })),
          });
        })
      : persistPresentation(next);
    void operation.catch((failure) => {
      setError(failure instanceof Error ? failure.message : String(failure));
    });
  }, [perform, persistPresentation, profile, scope]);

  const createProject = useCallback(async (draft: OctopusOsProjectDraft) => {
    if (profile !== "live") {
      const next = [...projectsRef.current, {
        id: crypto.randomUUID(),
        name: draft.name,
        sourceFolders: draft.sourceFolders,
        icon: draft.icon,
        color: draft.color,
        parentId: draft.parentId,
        chatRunIds: [],
      }];
      updatePresentation(next);
      return;
    }
    const parentEeId = draft.parentId.trim();
    if (!parentEeId) throw new Error("Project creation requires an explicit parent execution environment");
    const parent = parentEeId === scope.rootEeId
      ? scope.rootEnvironment
      : scope.projects.find((project) => project.eeId === parentEeId) ?? null;
    if (!parent?.recordDigest) throw new Error(`Project parent '${parentEeId}' has no projected record digest`);
    if (!parent.allowedChildTemplateIds.includes(draft.templateName)
      || !scope.templates.some((template) => template.id === draft.templateName)) {
      throw new Error("Choose an installed project policy allowed by the selected parent.");
    }
    await perform(async () => {
      await mutateOctopusOsWorkspace({
        action: "create_project",
        operationId: crypto.randomUUID(),
        workspace: workspaceMutationIdentity(scope),
        parentEeId,
        expectedParentDigest: parent.recordDigest,
        templateName: draft.templateName,
        name: draft.name,
        sourceFolders: draft.sourceFolders,
        goal: draft.name,
        presentation: { icon: draft.icon, color: draft.color },
      });
    });
  }, [perform, profile, scope, updatePresentation]);

  const editProject = useCallback(async (edit: OctopusOsProjectEdit) => {
    const current = projectsRef.current.find((project) => project.id === edit.projectId);
    if (!current) throw new Error(`Project '${edit.projectId}' is no longer available`);
    const next = projectsRef.current.map((project) => project.id === edit.projectId
      ? { ...project, name: edit.name, icon: edit.icon, color: edit.color, sourceFolders: edit.sourceFolders }
      : project);
    if (profile !== "live") {
      await persistPresentation(next);
      return;
    }
    await perform(async () => {
      await mutateOctopusOsWorkspace({
        action: "update_project_presentation",
        operationId: crypto.randomUUID(),
        workspace: workspaceMutationIdentity(scope),
        projects: next.map((project) => ({
          eeId: project.id,
          ...(project.id === edit.projectId ? {
            sourceFolders: edit.sourceFolders,
            expectedProjectDigest: current.recordDigest,
            ...(edit.name === current.name ? {} : { name: edit.name }),
          } : {}),
          icon: project.icon,
          color: project.color,
        })),
      });
    });
  }, [perform, persistPresentation, profile, scope]);

  const createAgentTemplate = useCallback(async (input: ClientAgentPresetInput) => {
    if (profile !== "live") throw new Error("Agent template authoring is unavailable in the read-only Lab workspace.");
    let workerInterfaceId = "";
    let agentId = "";
    await perform(async () => {
      const outcome = await mutateOctopusOsWorkspace({
        action: "create_agent_template",
        operationId: crypto.randomUUID(),
        workspace: agentWorkspaceMutationIdentity(scope),
        projectEeId: input.projectEeId,
        sourceWorkerInterfaceId: input.workerInterfaceId,
        name: input.name,
        icon: input.icon,
        providerInterfaceId: input.providerInterfaceId,
        providerModel: input.providerModel,
        reasoningEffort: input.reasoningEffort,
        serviceTier: input.serviceTier,
        applicationIds: input.applicationIds,
        driverNames: input.driverNames,
      });
      workerInterfaceId = outcome.workerInterfaceId ?? "";
      agentId = outcome.agentId ?? "";
      if (!workerInterfaceId || !agentId) throw new Error("Agent authoring returned no exact client and worker identity");
    });
    return { agentId, workerInterfaceId };
  }, [perform, profile, scope]);

  const configureAgentTemplate = useCallback(async (
    input: ClientAgentPresetInput,
    agentId: string,
    expectedClientTemplateDigest: string,
  ) => {
    if (profile !== "live") throw new Error("Agent template configuration is unavailable in the read-only Lab workspace.");
    await perform(async () => {
      await mutateOctopusOsWorkspace({
        action: "configure_agent_template",
        agentId,
        projectEeId: input.projectEeId,
        workerInterfaceId: input.workerInterfaceId,
        name: input.name,
        icon: input.icon,
        providerInterfaceId: input.providerInterfaceId,
        providerModel: input.providerModel,
        reasoningEffort: input.reasoningEffort,
        serviceTier: input.serviceTier,
        applicationIds: input.applicationIds,
        driverNames: input.driverNames,
        expectedClientTemplateDigest,
        expectedAgentCatalogDigest: scope.agentCatalogDigest,
      });
    });
  }, [perform, profile, scope.agentCatalogDigest]);

  const deleteAgentTemplate = useCallback(async (
    agentId: string,
    projectEeId: string,
    workerInterfaceId: string,
    expectedClientTemplateDigest: string,
  ) => {
    if (profile !== "live") throw new Error("Agent template deletion is unavailable in the read-only Lab workspace.");
    await perform(async () => {
      await mutateOctopusOsWorkspace({
        action: "delete_agent_template",
        agentId,
        projectEeId,
        workerInterfaceId,
        expectedClientTemplateDigest,
        expectedAgentCatalogDigest: scope.agentCatalogDigest,
      });
    });
  }, [perform, profile, scope.agentCatalogDigest]);

  const deleteProject = useCallback(async (projectId: string) => {
    const current = projectsRef.current.find((project) => project.id === projectId);
    if (!current) throw new Error(`Project '${projectId}' is no longer available`);
    if (profile !== "live") {
      const index = projectsRef.current.findIndex((project) => project.id === projectId);
      const reparentedChildIds = projectsRef.current
        .filter((project) => project.parentId === projectId)
        .map((project) => project.id);
      await persistPresentation(projectsRef.current
        .filter((project) => project.id !== projectId)
        .map((project) => project.parentId === projectId ? { ...project, parentId: current.parentId } : project));
      return { index, project: current, reparentedChildIds };
    }
    if (!current.recordDigest) throw new Error(`Project '${projectId}' has no projected record digest`);
    await perform(async () => {
      await mutateOctopusOsWorkspace({
        action: "delete_project",
        operationId: crypto.randomUUID(),
        workspace: workspaceMutationIdentity(scope),
        eeId: projectId,
        expectedProjectDigest: current.recordDigest ?? "",
      });
    });
    return null;
  }, [perform, persistPresentation, profile, scope]);

  const restoreProject = useCallback(async (receipt: OctopusOsProjectDeletionReceipt) => {
    if (profile === "live") throw new Error("Live execution-environment deletion cannot be restored from client presentation data.");
    if (projectsRef.current.some((project) => project.id === receipt.project.id)) return;
    if (
      receipt.project.parentId
      && receipt.project.parentId !== scope.rootEeId
      && !projectsRef.current.some((project) => project.id === receipt.project.parentId)
    ) {
      throw new Error(`Could not restore '${receipt.project.name}' because its original parent no longer exists.`);
    }
    const reparented = new Set(receipt.reparentedChildIds);
    const next = projectsRef.current.map((project) => (
      reparented.has(project.id) && project.parentId === receipt.project.parentId
        ? { ...project, parentId: receipt.project.id }
        : project
    ));
    next.splice(Math.max(0, Math.min(receipt.index, next.length)), 0, receipt.project);
    await persistPresentation(next);
  }, [persistPresentation, profile, scope.rootEeId]);

  const deleteTask = useCallback(async (runId: string) => {
    if (profile !== "live") throw new Error("Task topology deletion is available only in the live Octopus OS.");
    const expectedDigest = scope.taskDigests.get(runId);
    if (!expectedDigest) throw new Error(`Task '${runId}' has no projected record digest`);
    let cleanupWarnings: string[] = [];
    await perform(async () => {
      const outcome = await mutateOctopusOsWorkspace({
        action: "delete_task",
        runId,
        expectedDigest,
      });
      cleanupWarnings = outcome.cleanupWarnings ?? [];
    });
    if (cleanupWarnings.length) setError(cleanupWarnings.join("\n"));
  }, [perform, profile, scope.taskDigests]);

  const setTaskArchived = useCallback(async (runId: string, archived: boolean) => {
    if (profile !== "live") throw new Error("Task archiving is available only in the live Octopus OS.");
    const expectedDigest = scope.taskDigests.get(runId);
    if (!expectedDigest) throw new Error(`Task '${runId}' has no projected record digest`);
    await perform(async () => {
      await mutateOctopusOsWorkspace({ action: "set_task_archived", home: runtimeHome,
        workspace: { rootEeId: scope.rootEeId, homeRunId: scope.homeRunId, homeInterfaceId: scope.homeInterfaceId }, runId, archived, expectedDigest });
    });
  }, [perform, profile, runtimeHome, scope]);

  return {
    projects,
    rootEeId: scope.rootEeId,
    homeRunId: scope.homeRunId,
    homeInterfaceId: scope.homeInterfaceId,
    rootContext: scope.rootEnvironment ? {
      eeId: scope.rootEnvironment.eeId,
      displayName: scope.rootEnvironment.name || scope.rootEnvironment.eeId,
      templateName: scope.rootEnvironment.templateName,
    } : null,
    rootAllowedChildTemplateIds: scope.rootEnvironment?.allowedChildTemplateIds ?? [],
    templates: scope.templates,
    status: scope.status,
    phase: !loaded ? readError ? "failed" : "initial-loading" : readError ? "degraded" : reading ? "refreshing" : "ready",
    pending,
    error: error || readError,
    updatePresentation,
    createProject,
    createAgentTemplate,
    configureAgentTemplate,
    deleteAgentTemplate,
    editProject,
    deleteProject,
    restoreProject,
    deleteTask,
    setTaskArchived,
  };
}

function normalizeScope(value: unknown): ScopeSnapshot {
  const home = normalizeHomeProjection(value);
  if (!isRecord(value)) throw new Error("Octopus OS scope projection is not an object");
  const rootEnvironment = normalizeEnvironment(value.root_environment);
  const projects = list(value.projects).map(normalizeEnvironment).filter((row): row is ScopeEnvironment => row !== null);
  const projectPresentations = list(value.project_presentations)
    .map(normalizeProjectPresentation)
    .filter((row): row is ProjectPresentation => row !== null);
  const templates = list(value.ee_templates).flatMap((row): OctopusOsEeTemplateOption[] => {
    const option = projectPolicyOption(row);
    return option ? [option] : [];
  });
  const taskDigests = new Map<string, string>();
  for (const row of list(value.client_runs)) {
    if (!isRecord(row) || text(row.surface_role) !== "task") continue;
    const runId = text(row.run_id);
    const digest = text(row.record_digest);
    if (runId && digest) taskDigests.set(runId, digest);
  }
  for (const row of list(value.read_only_runs)) {
    if (!isRecord(row)) continue;
    const runId = text(row.run_id);
    const digest = text(row.record_digest);
    if (runId && digest) taskDigests.set(runId, digest);
  }
  if (["ready", "awaiting_home"].includes(home.status) && (!home.rootEeId || !rootEnvironment || rootEnvironment.eeId !== home.rootEeId)) {
    throw new Error("Octopus OS scope has no valid root execution environment");
  }
  return { ...home, rootEnvironment, projects, projectPresentations, templates, taskDigests };
}

function normalizeHomeProjection(value: unknown): Pick<ScopeSnapshot, "status" | "rootEeId" | "spaceName" | "homeRunId" | "homeInterfaceId" | "homeSidecarDigest" | "agentCatalogDigest"> {
  if (!isRecord(value)) throw new Error("Octopus OS scope projection is not an object");
  return {
    status: text(value.status) || "unbound",
    rootEeId: text(value.root_ee_id),
    spaceName: text(value.space_name),
    homeRunId: text(value.home_run_id),
    homeInterfaceId: text(value.home_interface_id),
    homeSidecarDigest: text(value.home_sidecar_digest),
    agentCatalogDigest: text(value.agent_catalog_digest),
  };
}

function agentWorkspaceMutationIdentity(scope: ScopeSnapshot) {
  const workspace = workspaceMutationIdentity(scope);
  if (!scope.agentCatalogDigest) {
    throw new Error("Octopus OS Client Agent catalog is not ready for Agent changes");
  }
  return {
    ...workspace,
    expectedAgentCatalogDigest: scope.agentCatalogDigest,
  };
}

function workspaceMutationIdentity(scope: ScopeSnapshot) {
  if (
    scope.status !== "ready"
    || !scope.rootEeId
    || !scope.homeRunId
    || !scope.homeInterfaceId
    || !scope.homeSidecarDigest
  ) {
    throw new Error("Octopus OS Home is not ready for Project changes");
  }
  return {
    rootEeId: scope.rootEeId,
    homeRunId: scope.homeRunId,
    homeInterfaceId: scope.homeInterfaceId,
    expectedHomeSidecarDigest: scope.homeSidecarDigest,
  };
}

function normalizeEnvironment(value: unknown): ScopeEnvironment | null {
  if (!isRecord(value)) return null;
  const eeId = text(value.ee_id);
  if (!eeId) return null;
  return {
    eeId,
    name: text(value.name),
    sourceFolders: list(isRecord(value.config) && isRecord(value.config.octopus_os) ? value.config.octopus_os.source_folders : []).map(text).filter(Boolean),
    parentEeId: text(value.parent_ee_id),
    templateName: text(value.template_name),
    goal: text(value.goal) || eeId,
    status: text(value.status),
    recordDigest: text(value.record_digest),
    allowedChildTemplateIds: list(value.allowed_child_ee_templates).map(text).filter(Boolean),
    clientRunIds: list(value.client_run_ids).map(text).filter(Boolean),
    readOnlyRunIds: list(value.read_only_run_ids).map(text).filter(Boolean),
  };
}

function normalizeProjectPresentation(value: unknown): ProjectPresentation | null {
  if (!isRecord(value)) return null;
  const eeId = text(value.ee_id);
  const name = text(value.name);
  if (!eeId || !name) return null;
  return {
    eeId,
    name,
    icon: text(value.icon),
    color: text(value.color),
  };
}

function mergeProjectedProjects(
  scope: ScopeSnapshot,
  presentation: readonly ClientChatProject[],
): ClientChatProject[] {
  const configured = new Map(presentation.map((project) => [project.id, project]));
  const canonical = new Map(scope.projects.map((environment) => [environment.eeId, environment]));
  const order = [
    ...presentation.map((project) => project.id).filter((id) => canonical.has(id)),
    ...scope.projects.map((environment) => environment.eeId).filter((id) => !configured.has(id)),
  ];
  return order.map((id) => {
    const environment = canonical.get(id)!;
    const visual = configured.get(id);
    return {
      id,
      sourceFolders: environment.sourceFolders,
      name: environment.name || environment.eeId,
      icon: visual?.icon || "briefcase",
      color: visual?.color || DEFAULT_PROJECT_COLOR,
      parentId: environment.parentEeId === scope.rootEeId ? "" : environment.parentEeId,
      chatRunIds: environment.clientRunIds,
      templateName: environment.templateName,
      runtimeStatus: environment.status,
      recordDigest: environment.recordDigest,
      allowedChildTemplateIds: environment.allowedChildTemplateIds,
      readOnlyRunIds: environment.readOnlyRunIds,
    };
  });
}

function projectPresentationRows(scope: ScopeSnapshot): ClientChatProject[] {
  return scope.projectPresentations.map((project) => ({
    id: project.eeId,
    name: project.name,
    icon: project.icon,
    color: project.color,
    parentId: "",
    chatRunIds: [],
  }));
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return Boolean(value) && typeof value === "object" && !Array.isArray(value);
}

function list(value: unknown): unknown[] {
  return Array.isArray(value) ? value : [];
}

function text(value: unknown): string {
  return typeof value === "string" ? value.trim() : "";
}
