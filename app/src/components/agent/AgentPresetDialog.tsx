import { useEffect, useMemo, useRef, useState, type FormEvent } from "react";
import type { ChatApplicationChoice, ChatExecutionContext, ChatUnavailableApplicationChoice, ChatWorkerChoice } from "../../chatRuns";
import type { OctopusProviderInterface } from "../../electron";
import {
  AGENT_PRESET_ICONS,
  DEFAULT_AGENT_PRESET_ICON,
  type ClientAgentDefinition,
  type ClientAgentPreset,
  type ClientAgentPresetIcon,
  type ClientAgentPresetInput,
} from "../../agentPresets";
import type { ClientChatProject } from "../../shell/clientWorkspaces";
import {
  OcButton,
  OcCheckbox,
  OcDialog,
  OcDialogFooter,
  OcDialogHeader,
  OcSelect,
  OcTextField,
} from "../../design-system/components";
import { AgentPresetLogo } from "./AgentPresetLogo";
import { Glyph } from "../IconGlyphs";
import {
  executionContextTaskCapabilities,
  providerModels,
  providerDefaultReasoningEffort,
  providerForExecutionContext,
  providerReasoningEfforts,
  providerServiceTiers,
  providerServiceTierAvailable,
} from "../chat/chatWorkspaceCapabilities";

type Props = {
  busy?: boolean;
  contexts: readonly ChatExecutionContext[];
  initialProjectEeId: string;
  open: boolean;
  preset: ClientAgentDefinition | null;
  presets: readonly ClientAgentDefinition[];
  projects: readonly ClientChatProject[];
  providers: readonly OctopusProviderInterface[];
  readOnly?: boolean;
  modelsLoading?: boolean;
  modelError?: string;
  onRefreshModels: () => Promise<void>;
  onCancel: () => void;
  onDelete: (presetId: string) => void;
  onSubmit: (value: ClientAgentPresetInput) => void;
};

const AGENT_PRESET_ICON_LABELS: Readonly<Record<ClientAgentPresetIcon, string>> = {
  default: "Default",
  strict: "Strict",
  calm: "Calm",
  curious: "Curious",
  glasses: "Glasses",
  goggles: "Engineering goggles",
  visor: "Visor",
  eyepiece: "Eyepiece",
  headphones: "Headphones",
  headset: "Operator headset",
  headband: "Headband",
  hardhat: "Hard hat",
};

export function AgentPresetDialog({ busy = false, contexts, initialProjectEeId, open, preset, presets, projects, providers, readOnly = false, modelsLoading = false, modelError = "", onRefreshModels, onCancel, onDelete, onSubmit }: Props) {
  const inputRef = useRef<HTMLInputElement>(null);
  const initializedDialogRef = useRef("");
  const [name, setName] = useState("");
  const [icon, setIcon] = useState<ClientAgentPresetIcon>(DEFAULT_AGENT_PRESET_ICON);
  const [projectEeId, setProjectEeId] = useState("");
  const [workerInterfaceId, setWorkerInterfaceId] = useState("");
  const [providerInterfaceId, setProviderInterfaceId] = useState("");
  const [providerModel, setProviderModel] = useState("");
  const [reasoningEffort, setReasoningEffort] = useState("");
  const [serviceTier, setServiceTier] = useState("default");
  const [applicationIds, setApplicationIds] = useState<string[]>([]);
  const [driverNames, setDriverNames] = useState<string[]>([]);
  const [deleteConfirm, setDeleteConfirm] = useState(false);
  const [openChoiceSection, setOpenChoiceSection] = useState<"applications" | "context" | "environments" | null>(null);
  const projectNameById = useMemo(() => new Map(projects.map((project) => [project.id, project.name])), [projects]);
  const projectContexts = contexts.filter((context) => projectNameById.has(context.eeId)
    || context.isOsRoot && context.workers.some((worker) => worker.agentSourceTemplate));
  const selectedProject = projects.find((project) => project.id === projectEeId) ?? null;
  const selectedContext = projectContexts.find((context) => context.eeId === projectEeId) ?? null;
  const selectedWorker = selectedContext?.workers.find((worker) => worker.id === workerInterfaceId) ?? null;
  const declaredSelectedProvider = providers.find((provider) => provider.id === providerInterfaceId) ?? null;
  const selectedProvider = selectedWorker && declaredSelectedProvider
    ? providerForExecutionContext(declaredSelectedProvider, selectedWorker)
    : null;
  const providerOptions = selectedWorker?.providerInterfaceIds.flatMap((id) => {
    const provider = providers.find((candidate) => candidate.id === id);
    if (!provider) return [];
    const formed = providerForExecutionContext(provider, selectedWorker);
    return providerModels(formed).map((model) => ({
      value: JSON.stringify([formed.id, model.model]),
      label: `${formed.sourceNames.join(" / ") || formed.id} · ${model.displayName || model.model}`,
      description: model.description || model.model,
    }));
  }) ?? [];
  const unavailableModels = providerOptions.length ? "" : selectedWorker?.providerInterfaceIds
    .map((id) => providers.find((provider) => provider.id === id)?.modelCatalogError).filter(Boolean).join("\n") ?? "";
  const reasoningOptions = selectedProvider && providerModel ? providerReasoningEfforts(selectedProvider, providerModel) : [];
  const applications = selectedWorker?.applications.filter((application) => application.category !== "artifact") ?? [];
  const contextArtifacts = selectedWorker?.applications.filter((application) => application.category === "artifact") ?? [];
  const unavailableApplications = selectedWorker?.unavailableApplications.filter((application) => application.category !== "artifact") ?? [];
  const unavailableContextArtifacts = selectedWorker?.unavailableApplications.filter((application) => application.category === "artifact") ?? [];
  const drivers = selectedWorker?.drivers ?? [];
  const invalidReason = modelsLoading ? "Refreshing available models…" : modelError || declaredSelectedProvider?.modelCatalogError || unavailableModels || validateSelection({
    applicationIds,
    context: selectedContext,
    contexts,
    driverNames,
    name,
    provider: selectedProvider,
    providerInterfaceId,
    providerModel,
    reasoningEffort,
    serviceTier,
    worker: selectedWorker,
  });
  useEffect(() => {
    if (open && !readOnly) void onRefreshModels();
  }, [open, readOnly, onRefreshModels]);
  useEffect(() => {
    if (!open) {
      initializedDialogRef.current = "";
      return;
    }
    const initializationKey = preset?.id || `new:${initialProjectEeId}`;
    if (initializedDialogRef.current === initializationKey) return;
    if (!preset && (!projectContexts.length || !providers.length)) return;
    initializedDialogRef.current = initializationKey;
    setDeleteConfirm(false);
    setOpenChoiceSection(null);
    setName(preset?.name ?? "");
    if (preset) {
      setIcon(preset.icon);
      setProjectEeId(preset.projectEeId);
      setWorkerInterfaceId(preset.workerInterfaceId);
      setProviderInterfaceId(preset.providerInterfaceId);
      setProviderModel(preset.providerModel);
      setReasoningEffort(preset.reasoningEffort);
      setServiceTier(preset.serviceTier || "default");
      setApplicationIds([...preset.applicationIds]);
      setDriverNames([...preset.driverNames]);
      return;
    }
    const requestedProject = projectContexts.some((context) => context.eeId === initialProjectEeId)
      ? initialProjectEeId
      : projectContexts.length === 1 ? projectContexts[0].eeId : "";
    const defaults = defaultsForContext(projectContexts.find((context) => context.eeId === requestedProject) ?? null, providers);
    setIcon(defaultAgentIconForProject(presets, requestedProject));
    setProjectEeId(requestedProject);
    setWorkerInterfaceId(defaults.workerInterfaceId);
    setProviderInterfaceId(defaults.providerInterfaceId);
    setProviderModel("");
    setReasoningEffort(defaults.reasoningEffort);
    setServiceTier("default");
    setApplicationIds(defaults.applicationIds);
    setDriverNames(defaults.driverNames);
  }, [initialProjectEeId, open, preset?.id, projectContexts.length, providers.length]);

  const chooseProject = (nextProjectEeId: string) => {
    const defaults = defaultsForContext(contexts.find((context) => context.eeId === nextProjectEeId) ?? null, providers);
    if (!preset) setIcon(defaultAgentIconForProject(presets, nextProjectEeId));
    setProjectEeId(nextProjectEeId);
    setWorkerInterfaceId(defaults.workerInterfaceId);
    setProviderInterfaceId(defaults.providerInterfaceId);
    setProviderModel("");
    setReasoningEffort(defaults.reasoningEffort);
    setServiceTier("default");
    setApplicationIds(defaults.applicationIds);
    setDriverNames(defaults.driverNames);
  };

  const chooseModel = (selection: string) => {
    const [nextProviderInterfaceId, nextModel] = JSON.parse(selection) as [string, string];
    const declared = providers.find((candidate) => candidate.id === nextProviderInterfaceId);
    if (!declared || !selectedWorker) return;
    const provider = providerForExecutionContext(declared, selectedWorker);
    if (!providerModels(provider).some((model) => model.model === nextModel)) return;
    setProviderInterfaceId(nextProviderInterfaceId);
    setProviderModel(nextModel);
    setReasoningEffort(providerDefaultReasoningEffort(provider, nextModel));
  };

  const submit = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    if (busy || readOnly || invalidReason) return;
    onSubmit({
      name: name.trim(),
      icon,
      projectEeId,
      workerInterfaceId,
      providerInterfaceId,
      providerModel,
      reasoningEffort,
      serviceTier,
      applicationIds,
      driverNames,
    });
  };

  return (
    <OcDialog
      ariaLabelledBy="octopus-agent-preset-title"
      initialFocusRef={readOnly ? undefined : inputRef}
      onClose={onCancel}
      open={open}
      panelClassName="agent-preset-dialog max-h-[min(780px,calc(100vh-48px))]"
      size="default"
    >
      <form className="flex min-h-0 flex-1 flex-col overflow-hidden px-5 py-5 text-base leading-normal tracking-normal" onSubmit={submit}>
        <OcDialogHeader
          className="p-0 pb-3"
          description={readOnly
            ? "This Lab Agent is projected fixture data. Agent authoring is available only in the live Octopus OS."
            : preset
            ? "Changes become defaults for future tasks. Existing runs keep their original configuration."
            : "Configure a reusable Agent for future tasks. Creating an Agent does not create a run."}
          onClose={onCancel}
          title={readOnly && preset ? preset.name : preset ? `Edit ${preset.name}` : "Create agent"}
          titleId="octopus-agent-preset-title"
        />
        <div className="vertical-scroll-fade-mask -mr-2 flex min-h-0 flex-1 flex-col gap-3 overflow-y-auto pr-2 [&>*]:shrink-0">
          <section className="flex w-full flex-col gap-2">
            <label className="text-base text-token-foreground" htmlFor="octopus-agent-name">Agent name</label>
            <div className="flex items-center gap-2.5">
              <span className="flex size-10 shrink-0 items-center justify-center rounded-xl bg-token-foreground/[0.035]">
                <AgentPresetLogo color={selectedProject?.color} icon={icon} size={34} />
              </span>
              <OcTextField
                className="min-w-0 flex-1"
                disabled={busy || readOnly}
                id="octopus-agent-name"
                inputRef={inputRef}
                label="Agent name"
                maxLength={80}
                onChange={(event) => setName(event.currentTarget.value)}
                placeholder="Research partner"
                value={name}
              />
            </div>
          </section>

          <section className="flex flex-col gap-2">
            <OcSelect
              disabled={busy || readOnly || Boolean(preset)}
              label="Project"
              onChange={chooseProject}
              options={projectContexts.map((context) => ({
                value: context.eeId,
                label: context.isOsRoot ? "No project" : projectNameById.get(context.eeId) || context.displayName || context.goal || context.eeId,
                description: context.goal || context.templateName || context.eeId,
              }))}
              placeholder="Choose a project"
              showLabel
              stretch
              value={projectEeId}
            />
          </section>

          <section className="flex flex-col gap-2">
            <div className="flex items-center justify-between gap-3">
              <span className="text-base text-token-foreground">Icon</span>
              <span className="truncate text-xs text-token-description-foreground">
                {selectedProject ? `${selectedProject.name} color` : "Project color"}
              </span>
            </div>
            <div className="grid grid-cols-6 gap-1.5" role="radiogroup" aria-label="Agent icon">
              {AGENT_PRESET_ICONS.map((option) => (
                <button
                  aria-checked={icon === option}
                  aria-label={AGENT_PRESET_ICON_LABELS[option]}
                  className={`no-drag cursor-interaction flex h-12 min-w-0 items-center justify-center rounded-xl border outline-none focus-visible:ring-2 focus-visible:ring-token-focus-border ${icon === option ? "border-token-foreground/55 bg-token-list-hover-background" : "border-transparent hover:bg-token-list-hover-background"}`}
                  disabled={busy || readOnly}
                  key={option}
                  onClick={() => setIcon(option)}
                  role="radio"
                  title={AGENT_PRESET_ICON_LABELS[option]}
                  type="button"
                >
                  <AgentPresetLogo color={selectedProject?.color} icon={option} size={32} />
                </button>
              ))}
            </div>
          </section>

          <section className="grid grid-cols-2 gap-3">
            <OcSelect
              disabled={busy || readOnly || modelsLoading}
              label="Model"
              onChange={chooseModel}
              options={providerOptions}
              placeholder="Choose a model"
              showLabel
              stretch
              value={providerModel ? JSON.stringify([providerInterfaceId, providerModel]) : ""}
            />
            <OcSelect
              disabled={busy || readOnly || reasoningOptions.length <= 1}
              label="Reasoning"
              onChange={setReasoningEffort}
              options={reasoningOptions.map((effort) => ({ value: effort, label: reasoningLabel(effort) }))}
              placeholder={reasoningOptions.length ? "Choose reasoning" : "Provider default"}
              showLabel
              stretch
              value={reasoningEffort}
            />
            <OcSelect disabled={busy || readOnly || !selectedProvider || !providerModel} label="Speed"
              onChange={setServiceTier} showLabel stretch value={serviceTier}
              placeholder="Choose speed"
              options={selectedProvider ? providerServiceTiers(selectedProvider, providerModel).map((tier) => ({ value: tier.id, label: tier.name })) : []} />
          </section>

          <AgentChoiceDisclosure
            description="Connectors and tools offered to each new task."
            empty="This Agent template exposes no applications in the selected project."
            items={applicationChoiceItems(applications, unavailableApplications)}
            open={openChoiceSection === "applications"}
            readOnly={busy || readOnly}
            selectedIds={applicationIds}
            title="Applications"
            onOpenChange={() => setOpenChoiceSection((current) => current === "applications" ? null : "applications")}
            onToggle={(id) => setApplicationIds((current) => toggleId(current, id))}
          />

          <AgentChoiceDisclosure
            description="Artifact bindings for skills, memory, and prepared context."
            empty="No skill, memory, or context artifact packages are available to this Agent template."
            items={applicationChoiceItems(contextArtifacts, unavailableContextArtifacts)}
            open={openChoiceSection === "context"}
            readOnly={busy || readOnly}
            selectedIds={applicationIds}
            title="Knowledge & memory"
            onOpenChange={() => setOpenChoiceSection((current) => current === "context" ? null : "context")}
            onToggle={(id) => setApplicationIds((current) => toggleId(current, id))}
          />

          <AgentChoiceDisclosure
            description="Execution surfaces prepared only when a task creates its real run."
            empty="This Agent template exposes no Worker Environments in the selected project."
            items={drivers.map((driver) => ({
              id: driver.name,
              label: displayName(driver.name),
              description: driver.capabilities.join(" · ") || `${driver.toolIds.length} tools`,
            }))}
            open={openChoiceSection === "environments"}
            readOnly={busy || readOnly}
            selectedIds={driverNames}
            title="Worker Environments"
            onOpenChange={() => setOpenChoiceSection((current) => current === "environments" ? null : "environments")}
            onToggle={(id) => setDriverNames((current) => toggleId(current, id))}
          />

          <aside className="flex items-start gap-2.5 rounded-xl bg-token-foreground/[0.035] p-3">
            <Glyph className="mt-0.5 shrink-0 text-token-description-foreground" name="lightbulb" size={17} />
            <p className="m-0 text-sm leading-5 text-token-description-foreground">
              These defaults are applied only when a new task starts. Applications, context, and Worker Environments can still be removed before sending.
              {" "}Choose Project access or Full access separately for each task in its composer.
            </p>
          </aside>

          {invalidReason ? <p className="m-0 rounded-xl bg-token-error-background/20 px-3 py-2 text-sm text-token-error-foreground" role="alert">{invalidReason}</p> : null}
        </div>
        <OcDialogFooter
          className="mt-3 min-h-0 border-token-border/40 px-0 pb-0 pt-4"
          leading={!readOnly && preset ? deleteConfirm ? (
            <span className="text-token-error-foreground">Delete this agent? Existing runs stay intact.</span>
          ) : (
            <OcButton disabled={busy} leadingIcon="trash" onClick={() => setDeleteConfirm(true)} tone="danger" variant="ghost">Delete agent</OcButton>
          ) : undefined}
        >
          {readOnly ? (
            <OcButton onClick={onCancel} variant="primary">Close</OcButton>
          ) : deleteConfirm && preset ? (
            <>
              <OcButton disabled={busy} onClick={() => setDeleteConfirm(false)} variant="ghost">Cancel</OcButton>
              <OcButton disabled={busy} onClick={() => onDelete(preset.id)} tone="danger" variant="secondary">Delete</OcButton>
            </>
          ) : (
            <>
              <OcButton disabled={busy} onClick={onCancel} variant="ghost">Cancel</OcButton>
              <OcButton disabled={busy || Boolean(invalidReason)} type="submit" variant="primary">{preset ? "Save" : "Create agent"}</OcButton>
            </>
          )}
        </OcDialogFooter>
      </form>
    </OcDialog>
  );
}

type AgentChoiceOption = {
  id: string;
  label: string;
  description: string;
  unavailable?: boolean;
};

function AgentChoiceDisclosure({ description, empty, items, open, readOnly, selectedIds, title, onOpenChange, onToggle }: {
  description: string;
  empty: string;
  items: readonly AgentChoiceOption[];
  open: boolean;
  readOnly: boolean;
  selectedIds: readonly string[];
  title: string;
  onOpenChange: () => void;
  onToggle: (id: string) => void;
}) {
  const choosableItems = items.filter((item) => !item.unavailable || selectedIds.includes(item.id));
  const selectedCount = items.filter((item) => selectedIds.includes(item.id)).length;
  return (
    <section className="overflow-hidden rounded-xl border border-token-border bg-token-main-surface-primary">
      <button
        aria-expanded={open}
        className="flex min-h-14 w-full cursor-interaction items-center gap-3 px-3 py-2.5 text-left outline-none hover:bg-token-foreground/[0.03] focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-token-focus-border"
        onClick={onOpenChange}
        type="button"
      >
        <span className="min-w-0 flex-1">
          <span className="block text-sm font-medium text-token-foreground">{title}</span>
          <span className="mt-0.5 block truncate text-xs leading-4 text-token-description-foreground">{description}</span>
        </span>
        <span className="shrink-0 text-sm text-token-description-foreground">
          {choosableItems.length ? `${selectedCount} of ${choosableItems.length}` : "None"}
        </span>
        <Glyph className={`shrink-0 transition-transform duration-basic ${open ? "rotate-180" : ""}`} name="chevronDown" size={15} />
      </button>
      {open ? (
        items.length ? (
          <div className="border-t border-token-border px-3 py-1">
            {items.map((item) => (
              <OcCheckbox
                checked={selectedIds.includes(item.id)}
                className="w-full py-2.5"
                description={item.description}
                disabled={readOnly || Boolean(item.unavailable) && !selectedIds.includes(item.id)}
                key={item.id}
                label={item.label}
                onCheckedChange={() => onToggle(item.id)}
                showLabel
              />
            ))}
          </div>
        ) : <AgentChoiceEmpty label={empty} />
      ) : null}
    </section>
  );
}

function AgentChoiceEmpty({ label }: { label: string }) {
  return <div className="border-t border-token-border px-3 py-4 text-center text-sm text-token-description-foreground">{label}</div>;
}

function defaultAgentIconForProject(
  presets: readonly ClientAgentPreset[],
  projectEeId: string,
): ClientAgentPresetIcon {
  const used = new Set(presets
    .filter((candidate) => candidate.projectEeId === projectEeId)
    .map((candidate) => candidate.icon));
  return AGENT_PRESET_ICONS.find((candidate) => !used.has(candidate)) ?? DEFAULT_AGENT_PRESET_ICON;
}

function defaultsForContext(context: ChatExecutionContext | null, providers: readonly OctopusProviderInterface[]) {
  const sourceWorkers = context?.workers.filter((worker) => worker.authoringProfile && worker.agentSourceTemplate) ?? [];
  const worker = sourceWorkers.length === 1 ? sourceWorkers[0] : null;
  return { workerInterfaceId: worker?.id ?? "", ...defaultsForWorker(worker, providers) };
}

function defaultsForWorker(worker: ChatWorkerChoice | null, providers: readonly OctopusProviderInterface[]) {
  const allowedProviders = worker
    ? providers
      .filter((provider) => worker.providerInterfaceIds.includes(provider.id))
      .map((provider) => providerForExecutionContext(provider, worker))
      .filter((provider) => provider.models.length > 0)
    : [];
  const preferredProviders = allowedProviders.filter((candidate) => candidate.preferred);
  const provider = preferredProviders.length === 1
    ? preferredProviders[0]
    : allowedProviders.length === 1
      ? allowedProviders[0]
      : null;
  return {
    providerInterfaceId: provider?.id ?? "",
    reasoningEffort: "",
    applicationIds: worker?.applications.map((application) => application.id).sort() ?? [],
    driverNames: worker?.drivers.map((driver) => driver.name).sort() ?? [],
  };
}

function validateSelection(input: {
  applicationIds: readonly string[];
  context: ChatExecutionContext | null;
  contexts: readonly ChatExecutionContext[];
  driverNames: readonly string[];
  name: string;
  provider: OctopusProviderInterface | null;
  providerInterfaceId: string;
  providerModel: string;
  reasoningEffort: string;
  serviceTier?: string;
  worker: ChatWorkerChoice | null;
}): string {
  if (!input.name.trim()) return "Enter an agent name.";
  if (!input.context) return "The selected project is not available in the current Octopus OS scope.";
  if (!input.worker) return "The selected Project has no reusable worker template.";
  if (
    !input.provider
    || !input.worker.providerInterfaceIds.includes(input.providerInterfaceId)
    || !input.providerModel
    || !providerModels(input.provider).some((model) => model.model === input.providerModel)
  ) return "Choose a model interface allowed by this Agent template.";
  if (!providerServiceTierAvailable(input.provider, input.providerModel, input.serviceTier ?? "")) return `Model '${input.providerModel}' does not advertise service tier '${input.serviceTier}'. Choose an available speed.`;
  const efforts = providerReasoningEfforts(input.provider, input.providerModel);
  if (efforts.length ? !efforts.includes(input.reasoningEffort) : Boolean(input.reasoningEffort)) return "The selected reasoning level is no longer supported by this model.";
  const applicationIds = new Set(input.worker.applications.map((application) => application.id));
  const missingApplication = input.applicationIds.find((id) => !applicationIds.has(id));
  if (missingApplication) return `Application '${missingApplication}' is no longer available to this Agent template.`;
  const driverNames = new Set(input.worker.drivers.map((driver) => driver.name));
  const missingDriver = input.driverNames.find((name) => !driverNames.has(name));
  if (missingDriver) return `Worker Environment '${missingDriver}' is no longer available to this Agent template.`;
  try {
    executionContextTaskCapabilities(
      input.contexts,
      input.context.eeId,
      input.worker.id,
      input.applicationIds,
      input.driverNames,
    );
  } catch (failure) {
    return failure instanceof Error ? failure.message : String(failure);
  }
  return "";
}

function toggleId(values: readonly string[], id: string): string[] {
  return values.includes(id) ? values.filter((value) => value !== id) : [...values, id].sort();
}

function applicationChoiceItems(
  available: readonly ChatApplicationChoice[],
  unavailable: readonly ChatUnavailableApplicationChoice[],
): AgentChoiceOption[] {
  return [
    ...available.map((application) => ({
      id: application.id,
      label: application.name,
      description: application.description || applicationBindingSummary(application),
    })),
    ...unavailable.map((application) => ({
      id: application.id,
      label: application.name,
      description: application.reason,
      unavailable: true,
    })),
  ];
}

function applicationBindingSummary(application: ChatApplicationChoice): string {
  const bindings = application.artifactBindings.length;
  const interfaces = application.interfaces.length;
  const tools = application.taskCapabilities.tools.length;
  return [
    bindings ? `${bindings} artifact ${bindings === 1 ? "binding" : "bindings"}` : "",
    interfaces ? `${interfaces} ${interfaces === 1 ? "interface" : "interfaces"}` : "",
    tools ? `${tools} ${tools === 1 ? "tool" : "tools"}` : "",
  ].filter(Boolean).join(" · ") || application.id;
}

function displayName(value: string): string {
  const words = value.trim().replace(/[_-]+/g, " ").replace(/\s+/g, " ");
  return words ? words[0].toLocaleUpperCase() + words.slice(1) : value;
}

function reasoningLabel(value: string): string {
  const normalized = displayName(value);
  return normalized === "Xhigh" ? "Extra high" : normalized;
}
