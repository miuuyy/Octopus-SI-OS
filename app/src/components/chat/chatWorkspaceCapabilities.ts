import type { ChatExecutionContext, ChatRun, ChatWorkerChoice } from "../../chatRuns";
import type { OctopusProviderInterface, OctopusProviderModel } from "../../electron";
import {
  HOST_FILESYSTEM_ROOT_SLOT,
  type TaskAccessMode,
} from "./taskAccess";

export type NewTaskDefaults = {
  eeId: string;
  workerInterfaceId: string;
  providerInterfaceId: string;
  providerModel: string;
  reasoningEffort: string;
  serviceTier?: string;
};

export function providerModels(provider: OctopusProviderInterface): OctopusProviderModel[] {
  return (provider.models ?? []).filter((model) => model.showInPicker || model.model === provider.model);
}

export function providerDefaultModel(provider: OctopusProviderInterface): OctopusProviderModel | null {
  const models = providerModels(provider);
  if (provider.model) return models.find((model) => model.model === provider.model) ?? null;
  return models.find((model) => model.model === provider.model)
    ?? models.find((model) => model.isDefault)
    ?? models[0]
    ?? null;
}

export function providerSelectedModel(
  provider: OctopusProviderInterface,
  model: string,
): OctopusProviderModel | null {
  const requested = model.trim();
  if (!requested) return providerDefaultModel(provider);
  return providerModels(provider).find((candidate) => candidate.model === requested) ?? null;
}

export function providerReasoningEfforts(
  provider: OctopusProviderInterface,
  model = provider.model ?? "",
): string[] {
  const selected = providerSelectedModel(provider, model);
  if (!selected) return [];
  const configured = selected.supportedReasoningEfforts.length
    ? selected.supportedReasoningEfforts
    : selected.defaultReasoningEffort
      ? [selected.defaultReasoningEffort]
      : [];
  return [...new Set(configured)];
}

export function providerServiceTiers(provider: OctopusProviderInterface, model: string) {
  return [
    { id: "default", name: "Standard", description: "Default speed" },
    ...(providerSelectedModel(provider, model)?.serviceTiers ?? []),
  ];
}

export function providerServiceTierAvailable(provider: OctopusProviderInterface, model: string, tier: string) {
  return providerServiceTiers(provider, model).some((option) => option.id === providerSelectedServiceTier(provider, model, tier));
}

export function providerSelectedServiceTier(provider: OctopusProviderInterface, model: string, tier = "") {
  return tier || provider.serviceTier || providerSelectedModel(provider, model)?.defaultServiceTier || "default";
}

export function providerDefaultReasoningEffort(
  provider: OctopusProviderInterface,
  model: string,
): string {
  const selected = providerSelectedModel(provider, model);
  if (!selected) return "";
  const options = providerReasoningEfforts(provider, selected.model);
  return selected.defaultReasoningEffort && options.includes(selected.defaultReasoningEffort)
    ? selected.defaultReasoningEffort
    : options[0] ?? "";
}

export function providerForExecutionContext(
  provider: OctopusProviderInterface,
  worker: ChatWorkerChoice,
): OctopusProviderInterface {
  const formedModels = worker.interfacePolicy[provider.id]?.models;
  if (!Array.isArray(formedModels) || formedModels.some((model) => typeof model !== "string" || !model)) {
    return { ...provider, model: "", models: [] };
  }
  const admitted = new Set(formedModels);
  const models = providerModels(provider)
    .filter((model) => admitted.has(model.model))
    .map((model) => ({ ...model }));
  return {
    ...provider,
    model: admitted.has(provider.model ?? "") ? provider.model : "",
    models,
  };
}

export function resolveNewTaskDefaults(
  runs: readonly ChatRun[],
  settings: { providerInterfaceId: string; providerModel: string; reasoningEffort: string; serviceTier?: string },
): NewTaskDefaults | null {
  const homes = runs.filter((run) => run.clientSurfaceRole === "home");
  if (homes.length !== 1) return null;
  const home = homes[0];
  const workerInterfaceId = home.workerChoiceInterfaceId || home.workerInterfaceId;
  if (
    !home.writable
    || !home.osRootEeId
    || home.eeId !== home.osRootEeId
    || !workerInterfaceId
    || !home.providerInterfaceId
  ) return null;
  const providerOverride = settings.providerInterfaceId.trim();
  const modelOverride = settings.providerModel.trim();
  const reasoningOverride = settings.reasoningEffort.trim();
  const providerInterfaceId = providerOverride || home.providerInterfaceId;
  const sameProvider = !providerOverride || providerOverride === home.providerInterfaceId;
  return {
    eeId: home.osRootEeId,
    workerInterfaceId,
    providerInterfaceId,
    providerModel: modelOverride || (sameProvider ? home.providerModel : ""),
    reasoningEffort: reasoningOverride
      || (sameProvider ? home.reasoningEffort : ""),
    serviceTier: settings.serviceTier || (sameProvider ? home.serviceTier : "") || "",
  };
}

export function executionContextAllowsProvider(
  contexts: readonly ChatExecutionContext[],
  eeId: string,
  workerInterfaceId: string,
  providerInterfaceId: string,
): boolean {
  const worker = contexts
    .find((context) => context.eeId === eeId)
    ?.workers.find((candidate) => candidate.id === workerInterfaceId);
  const models = worker?.interfacePolicy[providerInterfaceId]?.models;
  return Boolean(
    worker?.providerInterfaceIds.includes(providerInterfaceId)
    && Array.isArray(models)
    && models.length > 0
    && models.every((model) => typeof model === "string" && Boolean(model)),
  );
}

export function providerInExecutionContext(
  contexts: readonly ChatExecutionContext[],
  eeId: string,
  workerInterfaceId: string,
  provider: OctopusProviderInterface,
): OctopusProviderInterface | null {
  const worker = contexts
    .find((context) => context.eeId === eeId)
    ?.workers.find((candidate) => candidate.id === workerInterfaceId);
  if (!worker?.providerInterfaceIds.includes(provider.id)) return null;
  const formed = providerForExecutionContext(provider, worker);
  return formed.models.length ? formed : null;
}

export function providerForExistingRun(
  provider: OctopusProviderInterface,
  run: ChatRun | undefined,
  selection: { runId: string; eeId: string; workerInterfaceId: string },
): OctopusProviderInterface | null {
  const workerChoiceInterfaceId = run?.workerChoiceInterfaceId || run?.workerInterfaceId;
  if (!run || !run.writable || run.archived || !run.providerBoundInterfaceId
    || run.runId !== selection.runId || run.eeId !== selection.eeId
    || !selection.workerInterfaceId
    || (selection.workerInterfaceId !== workerChoiceInterfaceId && selection.workerInterfaceId !== run.workerInterfaceId)
    || run.providerInterfaceId !== provider.id) return null;
  return provider;
}

export function defaultCapabilitySelection(
  contexts: readonly ChatExecutionContext[],
  eeId: string,
  workerInterfaceId: string,
): { applicationIds: string[]; driverNames: string[] } {
  const worker = contexts
    .find((context) => context.eeId === eeId)
    ?.workers.find((candidate) => candidate.id === workerInterfaceId);
  return {
    applicationIds: worker?.applications.filter((application) => application.selectedByDefault).map((application) => application.id).sort() ?? [],
    driverNames: worker?.drivers.filter((driver) => driver.selectedByDefault).map((driver) => driver.name).sort() ?? [],
  };
}

export function taskCapabilitySelectionFailure(
  worker: ChatWorkerChoice | null,
  applicationIds: readonly string[],
  driverNames: readonly string[],
): string {
  if (!worker) return "The selected Agent worker template is not projected.";
  const applicationById = new Map(worker.applications.map((application) => [application.id, application]));
  const driverByName = new Map(worker.drivers.map((driver) => [driver.name, driver]));
  const unavailableApplicationId = applicationIds.find((id) => !applicationById.has(id));
  if (unavailableApplicationId) return `Application '${unavailableApplicationId}' is not available to this task.`;
  const unavailableDriverName = driverNames.find((name) => !driverByName.has(name));
  if (unavailableDriverName) return `Worker Environment '${unavailableDriverName}' is not available to this task.`;
  const selectedDriverNames = new Set(driverNames);
  for (const applicationId of applicationIds) {
    const application = applicationById.get(applicationId);
    const missingKind = application?.taskCapabilities.tools
      .map((capability) => capability.workerEnvironmentKind)
      .find((kind) => !selectedDriverNames.has(kind));
    if (missingKind) {
      return `Application '${application?.name || applicationId}' requires Worker Environment '${missingKind}'. Select that Worker Environment or remove the Application.`;
    }
  }
  return "";
}

export function taskFullAccessSelectionFailure(
  context: ChatExecutionContext | null,
  worker: ChatWorkerChoice | null,
  applicationIds: readonly string[],
  driverNames: readonly string[],
): string {
  if (!context || !worker) return "Select an Agent before enabling full access.";
  try {
    executionContextTaskCapabilities([context], context.eeId, worker.id, applicationIds, driverNames, "full_access");
    return "";
  } catch (failure) {
    return failure instanceof Error ? failure.message : String(failure);
  }
}

export function executionContextTaskCapabilities(
  contexts: readonly ChatExecutionContext[],
  eeId: string,
  workerInterfaceId: string,
  applicationIds: readonly string[],
  driverNames: readonly string[],
  accessMode: TaskAccessMode = "project_access",
) {
  const context = contexts.find((candidate) => candidate.eeId === eeId);
  const worker = context?.workers.find((candidate) => candidate.id === workerInterfaceId);
  if (!worker) {
    throw new Error(`Agent worker template '${workerInterfaceId}' is not projected in EE '${eeId}'.`);
  }
  const selectionFailure = taskCapabilitySelectionFailure(worker, applicationIds, driverNames);
  if (selectionFailure) throw new Error(selectionFailure);
  const applicationById = new Map(worker.applications.map((application) => [application.id, application]));
  const driverByName = new Map(worker.drivers.map((driver) => [driver.name, driver]));
  const selectedApplications = [...new Set(applicationIds)].map((id) => {
    const application = applicationById.get(id);
    if (!application) throw new Error(`Application '${id}' is not available to this task.`);
    return application;
  });
  const selectedDrivers = [...new Set(driverNames)].map((name) => {
    const driver = driverByName.get(name);
    if (!driver) throw new Error(`Worker Environment '${name}' is not available to this task.`);
    return driver;
  });
  const externalInputs = worker.externalInputs.map((input) => ({
    ...input,
    ...(input.artifactNames ? { artifactNames: [...input.artifactNames] } : {}),
    requiredVerifiedArtifacts: input.requiredVerifiedArtifacts.map((requirement) => ({
      ...requirement,
      params: { ...requirement.params },
    })),
  }));
  const externalRelays = worker.externalRelays.map((relay) => ({
    ...relay,
    outboundInterfaceIds: [...relay.outboundInterfaceIds],
    requiredVerifiedArtifacts: relay.requiredVerifiedArtifacts.map((requirement) => ({
      ...requirement,
      params: { ...requirement.params },
    })),
  }));
  const modelOutputs = worker.modelOutputs.map((output) => ({
    ...output,
    requiredVerifiedArtifact: {
      ...output.requiredVerifiedArtifact,
      params: { ...output.requiredVerifiedArtifact.params },
    },
  }));
  const selectedDriverNames = new Set(selectedDrivers.map((driver) => driver.name));
  const selectedTools = new Map<string, {
    tool: string;
    interface: string;
    workerEnvironmentKind: string;
  }>(worker.selectedTools
    .filter((selection) => selectedDriverNames.has(selection.workerEnvironmentKind))
    .map((selection) => [selection.tool, { ...selection }]));
  const contextArtifactBindings = worker.contextArtifactBindings.map((binding) => ({ ...binding }));
  const inputInterfaces = new Set(externalInputs.map((input) => input.interfaceId));
  const artifactIds = new Set(contextArtifactBindings.map((binding) => binding.id));
  const checkpointIds = new Set([
    ...externalInputs.map((input) => input.checkpointId),
    ...externalRelays.map((relay) => relay.checkpointId),
    ...modelOutputs.map((output) => output.checkpointId),
  ]);
  for (const driver of selectedDrivers) {
    for (const capability of driver.tools) {
      const existing = selectedTools.get(capability.tool);
      if (existing && (
        existing.interface !== capability.interface
        || existing.workerEnvironmentKind !== capability.workerEnvironmentKind
      )) {
        throw new Error(`Worker Environment tool '${capability.tool}' has more than one exact owner.`);
      }
      selectedTools.set(capability.tool, capability);
    }
  }
  for (const application of selectedApplications) {
    application.interfaces.forEach((interfaceId, index) => {
      if (inputInterfaces.has(interfaceId)) return;
      const checkpointId = applicationCheckpointId(application.id, "input", index);
      if (checkpointIds.has(checkpointId)) throw new Error(`Application checkpoint '${checkpointId}' is already authored.`);
      checkpointIds.add(checkpointId);
      inputInterfaces.add(interfaceId);
      externalInputs.push({ checkpointId, interfaceId, requiredVerifiedArtifacts: [] });
    });
    for (const binding of application.artifactBindings) {
      if (artifactIds.has(binding.artifactId)) continue;
      artifactIds.add(binding.artifactId);
      contextArtifactBindings.push({ id: binding.artifactId });
    }
    for (const capability of application.taskCapabilities.tools) {
      const existing = selectedTools.get(capability.tool);
      if (existing && (
        existing.interface !== capability.interface
        || existing.workerEnvironmentKind !== capability.workerEnvironmentKind
      )) {
        throw new Error(`Application tool '${capability.tool}' has more than one exact owner.`);
      }
      selectedTools.set(capability.tool, capability);
    }
  }
  const selectedDoorInterfaces = new Set(selectedDrivers.flatMap((driver) => driver.doorInterfaces));
  const doorHostSlots: Record<string, Record<string, string>> = {};
  for (const [door, slots] of Object.entries(worker.doorHostSlots)) {
    if (!selectedDoorInterfaces.has(door)) continue;
    doorHostSlots[door] = { ...(doorHostSlots[door] ?? {}), ...slots };
  }
  const projectSlotOwners = new Map<string, string>();
  for (const driver of selectedDrivers) {
    for (const door of driver.doorInterfaces) {
      const slots = doorHostSlots[door] ?? {};
      for (const [slot, root] of Object.entries(driver.hostSlots)) {
        const key = `${door}\0${slot}`;
        const owner = projectSlotOwners.get(key);
        if (owner && slots[slot] !== root) {
          throw new Error(`Worker Environments '${owner}' and '${driver.name}' configure different roots for '${slot}' on '${door}'.`);
        }
        slots[slot] = root;
        projectSlotOwners.set(key, driver.name);
      }
      if (Object.keys(slots).length) doorHostSlots[door] = slots;
    }
  }
  for (const driver of selectedDrivers.filter((driver) => driver.rootSlots.includes("workspace_root"))) {
    for (const door of driver.doorInterfaces) {
      if (context?.sourceFolders?.length) {
        doorHostSlots[door] = { ...doorHostSlots[door], workspace_root: context.sourceFolders[0] };
      }
    }
  }
  const hostRootDrivers = accessMode === "full_access"
    ? selectedDrivers.filter((candidate) => candidate.rootSlots.includes(HOST_FILESYSTEM_ROOT_SLOT))
    : [];
  if (hostRootDrivers.length) {
    const hostFilesystemRoot = context?.fullAccessRoot;
    if (!hostFilesystemRoot) {
      throw new Error("The host filesystem root is unavailable. Reload the task configuration.");
    }
    for (const driver of hostRootDrivers) {
      for (const door of driver.doorInterfaces) {
        const slots = doorHostSlots[door] ?? {};
        const configured = slots[HOST_FILESYSTEM_ROOT_SLOT];
        if (configured && configured !== hostFilesystemRoot) {
          throw new Error(
            `Worker Environment '${driver.name}' configures '${HOST_FILESYSTEM_ROOT_SLOT}' as '${configured}', not the host root '${hostFilesystemRoot}'.`,
          );
        }
        slots[HOST_FILESYSTEM_ROOT_SLOT] = hostFilesystemRoot;
        if (driver.rootSlots.includes("workspace_root") && !slots.workspace_root) slots.workspace_root = hostFilesystemRoot;
        doorHostSlots[door] = slots;
      }
    }
  }
  return {
    doorHostSlots,
    workspaceSelectionError: selectedDrivers.some((driver) => driver.rootSlots.includes("workspace_root")
      && driver.doorInterfaces.some((door) => !doorHostSlots[door]?.workspace_root && !context?.hostRootRefs.workspace_root))
      ? "This task has no working folder. Choose a project folder or change the task's access settings."
      : "",
    externalInputs,
    externalRelays,
    modelOutputs,
    contextArtifactBindings,
    payloadArtifactBindings: worker.payloadArtifactBindings.map((ref) => ({ id: ref.id, content: [...ref.content] })),
    selectedWorkerEnvironmentKinds: selectedDrivers.map((driver) => driver.name).sort(),
    selectedTools: [...selectedTools.values()].sort((left, right) => left.tool.localeCompare(right.tool)),
  };
}

function applicationCheckpointId(applicationId: string, direction: "input" | "output", index: number): string {
  const id = `cp_app_${applicationId}_${direction}_${index + 1}`;
  if (id.length > 96 || !/^[A-Za-z0-9_-]+$/.test(id)) {
    throw new Error(`Application '${applicationId}' cannot form a portable checkpoint id.`);
  }
  return id;
}
