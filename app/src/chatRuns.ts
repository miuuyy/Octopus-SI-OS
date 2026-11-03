import type { VoiceCallBinding } from "./components/voice/voiceCallArtifacts";
import { formatProjectionError, readProjection } from "./projectionClient";
import { normalizeTaskAccessMode, type TaskAccessMode } from "./components/chat/taskAccess";

export const OPERATOR_CHAT_INTERFACE = "world_operator_chat";
export const OPERATOR_CHAT_INBOUND_LABEL = "operator.message";
export const OPERATOR_CHAT_OUTBOUND_LABEL = "interface.artifact.output";

export type ChatRun = {
  voiceContext?: VoiceCallBinding | null;
  agentColor?: string;
  runId: string;
  title: string;
  titleGenerationNeeded?: boolean;
  chatInterfaceId: string;
  inputInterfaceIds?: string[];
  clientSurfaceRole: "home" | "task";
  writable: boolean;
  archived: boolean;
  osRootEeId: string;
  eeId: string;
  eeGoal: string;
  eeTemplateName: string;
  turnOpen: boolean;
  turnState: "idle" | "waiting" | "working" | "failed";
  providerInterfaceId: string;
  providerBoundInterfaceId: string;
  providerModel: string;
  providerModelsRefreshable: boolean;
  reasoningEffort: string;
  serviceTier?: string;
  workerInterfaceId: string;
  workerChoiceInterfaceId: string;
  doorHostSlots: Record<string, Record<string, string>>;
  createdAtMs: number;
  updatedAtMs: number;
  lastMovementSeq: number;
  nextScheduledAtMs: number;
  messageCount: number;
  pinnedArtifactIds: string[];
  pinnedContextItems: ChatPinnedContextItem[];
  continuedFrom: {
    runId: string;
    artifactId: string;
  } | null;
  recordDigest: string;
  connectedApplicationIds: string[];
  lastTerminalMessage: ChatTerminalMessage | null;
};

export type ChatRunStatus = Pick<
  ChatRun,
  "runId" | "archived" | "turnOpen" | "turnState" | "updatedAtMs" | "lastMovementSeq"
>;

export type ChatTerminalMessage = {
  artifactId: string;
  createdAtMs: number;
  interfaceId: string;
  movementSeq: number;
  text: string;
};

export type ChatExecutionContext = {
  sourceFolders: string[];
  fullAccessRoot: string;
  eeId: string;
  agentCatalogDigest: string;
  defaultTaskAccessMode: TaskAccessMode;
  hostRootRefs: Record<string, string>;
  isOsRoot: boolean;
  presentationColor: string;
  presentationIcon?: string;
  displayName: string;
  goal: string;
  templateName: string;
  parentEeId: string;
  workers: ChatWorkerChoice[];
};

export type ChatWorkerChoice = {
  id: string;
  label: string;
  authoringProfile: boolean;
  agentSourceTemplate: boolean;
  clientAgentId: string;
  clientAgentName: string;
  clientAgentIcon: string;
  clientAgentCreatedAtMs: number;
  clientAgentUpdatedAtMs: number;
  clientAgentDeletedAtMs: number;
  clientTemplate: boolean;
  clientTemplateProviderInterfaceId: string;
  clientTemplateProviderModel: string;
  clientTemplateReasoningEffort: string;
  clientTemplateServiceTier?: string;
  clientTemplateDigest: string;
  clientTemplateApplicationIds: string[];
  clientTemplateWorkerEnvironmentKinds: string[];
  providerInterfaceIds: string[];
  interfacePolicy: Record<string, Record<string, unknown>>;
  roleArtifactIds: string[];
  authoringRoomInterfaceId: string;
  doorHostSlots: Record<string, Record<string, string>>;
  externalInputs: ChatExternalInput[];
  externalRelays: ChatExternalRelay[];
  modelOutputs: ChatModelOutput[];
  contextArtifactBindings: Array<{ id: string }>;
  payloadArtifactBindings: Array<{ id: string; content: number[] }>;
  selectedWorkerEnvironmentKinds: string[];
  selectedTools: ChatApplicationToolCapability[];
  applications: ChatApplicationChoice[];
  unavailableApplications: ChatUnavailableApplicationChoice[];
  drivers: ChatDriverChoice[];
};

export type ChatExternalInput = {
  checkpointId: string;
  interfaceId: string;
  artifactNames?: string[];
  requiredVerifiedArtifacts: ChatRequiredVerifiedArtifact[];
};

export type ChatExternalRelay = {
  checkpointId: string;
  interfaceId: string;
  outboundInterfaceIds: string[];
  requiredVerifiedArtifacts: ChatRequiredVerifiedArtifact[];
};

export type ChatRequiredVerifiedArtifact = {
  mark: string;
  artifactLabel: string;
  verifier: string;
  params: Record<string, unknown>;
};

export type ChatModelOutput = {
  checkpointId: string;
  interfaceId: string;
  requiredVerifiedArtifact: {
    mark: string;
    artifactLabel: string;
    verifier: string;
    params: Record<string, unknown>;
  };
};

export type ChatApplicationChoice = {
  id: string;
  name: string;
  description: string;
  category: "connector" | "plugin" | "artifact";
  icon: string;
  logoDataUrl: string;
  logoDarkDataUrl: string;
  interfaces: string[];
  artifactBindings: Array<{ artifactId: string; label: string }>;
  taskCapabilities: {
    tools: ChatApplicationToolCapability[];
  };
  selectedByDefault: boolean;
};

export type ChatUnavailableApplicationChoice = ChatApplicationChoice & {
  reason: string;
};

export type ChatApplicationToolCapability = {
  tool: string;
  interface: string;
  workerEnvironmentKind: string;
};

export type ChatDriverChoice = {
  name: string;
  capabilities: string[];
  rootSlots: string[];
  toolIds: string[];
  doorInterfaces: string[];
  tools: ChatApplicationToolCapability[];
  hostSlots: Record<string, string>;
  selectedByDefault: boolean;
};

export type ChatWorkerEnvironmentRoom = {
  workerEnvironmentId: string;
  runId: string;
  eeId: string;
  workerInterfaceId: string;
  doorInterfaceId: string;
  kind: string;
  live: boolean;
  status: string;
  lifecycle: string;
  root: string;
  lastActivityMovementSeq: number;
};

export type ChatPinnedContextItem = {
  artifactId: string;
  sourceArtifactId: string;
  sourceMessageId: string;
  text: string;
  start: number;
  end: number;
  role: "user" | "assistant" | "system";
};

export type ChatRunAttachment = {
  id: string;
  name: string;
  size: number;
  type: string;
  label: string;
  lastModified: number;
};

export type ChatRunMessage = {
  id: string;
  runId: string;
  role: "user" | "assistant" | "system";
  label: string;
  direction: "in" | "out";
  text: string;
  createdAtMs: number;
  movementSeq: number;
  artifactId: string;
  contentHash: string;
  interfaceId: string;
  eventKind: ChatRunEventKind;
  eventData: Record<string, unknown>;
  latencyMs: number;
  attachments: ChatRunAttachment[];
  error: boolean;
  terminal: boolean;
};

export type ChatTaskMessageSearchMatch = {
  taskRunId: string;
  sourceRunId: string;
  snippet: string;
  message: ChatRunMessage;
};

export type ChatTaskMessageSearchPage = {
  matches: ChatTaskMessageSearchMatch[];
  hasOlder: boolean;
  nextBeforeSeq: number;
  indexWatermark: number;
  scope: {
    kind: "octopus_os_task_transcripts";
    taskCount: number;
    runCount: number;
    messageClasses: string[];
  };
};

export type ChatRunEventKind =
  | "message"
  | "steering"
  | "message_delta"
  | "commentary"
  | "commentary_delta"
  | "reasoning"
  | "reasoning_delta"
  | "plan"
  | "plan_delta"
  | "usage"
  | "tool"
  | "tool_output_delta"
  | "terminal_interaction"
  | "file_change"
  | "file_change_delta"
  | "diff"
  | "web_search"
  | "hook"
  | "hook_prompt"
  | "mcp_tool"
  | "mcp_progress"
  | "mcp_status"
  | "dynamic_tool"
  | "collaboration"
  | "subagent_activity"
  | "image_view"
  | "image_generation"
  | "sleep"
  | "review_entered"
  | "review_exited"
  | "compaction"
  | "item_started"
  | "turn_started"
  | "turn_completed"
  | "model_rerouted"
  | "model_verification"
  | "auth_recovery_started"
  | "auth_recovery_completed"
  | "warning"
  | "auto_review_started"
  | "auto_review_completed"
  | "operator_request_resolved"
  | "turn_control"
  | "input_recorded"
  | "interrupted_result"
  | "rate_limits"
  | "goal"
  | "approval"
  | "elicitation"
  | "user_input"
  | "error"
  | "generic_activity"
  | "event";

type RawChatRunRow = {
  voice_context?: unknown;
  agent_color?: unknown;
  run_id?: unknown;
  title?: unknown;
  title_generation_needed?: unknown;
  chat_interface_id?: unknown;
  input_interface_ids?: unknown;
  client_surface_role?: unknown;
  writable?: unknown;
  archived?: unknown;
  os_root_ee_id?: unknown;
  ee_id?: unknown;
  ee_goal?: unknown;
  ee_template_name?: unknown;
  turn_open?: unknown;
  turn_state?: unknown;
  provider_interface_id?: unknown;
  provider_bound_interface_id?: unknown;
  provider_model?: unknown;
  provider_models_refreshable?: unknown;
  reasoning_effort?: unknown;
  service_tier?: unknown;
  worker_interface_id?: unknown;
  worker_choice_interface_id?: unknown;
  door_host_slots?: unknown;
  created_at_ms?: unknown;
  updated_at_ms?: unknown;
  last_movement_seq?: unknown;
  next_scheduled_at_ms?: unknown;
  message_count?: unknown;
  pinned_artifact_ids?: unknown;
  pinned_context_items?: unknown;
  continued_from?: unknown;
  record_digest?: unknown;
  connected_application_ids?: unknown;
  last_terminal_message?: unknown;
};

type RawChatAttachmentRow = {
  id?: unknown;
  name?: unknown;
  size?: unknown;
  type?: unknown;
  label?: unknown;
  last_modified?: unknown;
};

type RawChatMessageRow = {
  id?: unknown;
  run_id?: unknown;
  role?: unknown;
  label?: unknown;
  direction?: unknown;
  text?: unknown;
  created_at_ms?: unknown;
  movement_seq?: unknown;
  artifact_id?: unknown;
  content_hash?: unknown;
  interface_id?: unknown;
  event_kind?: unknown;
  event_data?: unknown;
  latency_ms?: unknown;
  attachments?: unknown;
  error?: unknown;
  terminal?: unknown;
};

function asString(value: unknown): string {
  return typeof value === "string" ? value : "";
}

function asNumber(value: unknown): number {
  return typeof value === "number" && Number.isFinite(value) ? value : 0;
}

function asRole(value: unknown): ChatRunMessage["role"] {
  return value === "user" || value === "assistant" || value === "system" ? value : "assistant";
}

function asDirection(value: unknown): ChatRunMessage["direction"] {
  return value === "in" ? "in" : "out";
}

function asVoiceCallBinding(value: unknown): VoiceCallBinding | null {
  if (value == null) return null;
  if (typeof value !== "object" || Array.isArray(value)) throw new Error("Invalid Home voice binding projection.");
  const source = value as Record<string, unknown>;
  const keys = ["control_interface_id", "provider_id", "delegation_interface_id", "run_id"] as const;
  if (keys.some((key) => typeof source[key] !== "string" || !source[key])) throw new Error("Incomplete Home voice binding projection.");
  return { controlInterfaceId: source.control_interface_id as string, providerId: source.provider_id as string, delegationInterfaceId: source.delegation_interface_id as string, runId: source.run_id as string };
}

export async function loadChatRuns(): Promise<ChatRun[]> {
  const response = await readProjection<{ chats?: RawChatRunRow[] }>("projection.operator.chats.list");
  return projectChatRuns(response);
}

export function projectChatRuns(response: { chats?: RawChatRunRow[] }): ChatRun[] {
  const rows = Array.isArray(response?.chats) ? response.chats : [];
  return rows
    .map((row) => ({
      runId: asString(row.run_id),
      title: asString(row.title) || asString(row.run_id) || "Chat",
      titleGenerationNeeded: row.title_generation_needed === true,
      chatInterfaceId: asString(row.chat_interface_id),
      inputInterfaceIds: Array.isArray(row.input_interface_ids) ? row.input_interface_ids.map(asString).filter(Boolean) : [],
      clientSurfaceRole: row.client_surface_role === "home" ? ("home" as const) : ("task" as const),
      writable: row.writable === true,
      archived: row.archived === true,
      osRootEeId: asString(row.os_root_ee_id),
      voiceContext: asVoiceCallBinding(row.voice_context),
      agentColor: asString(row.agent_color),
      eeId: asString(row.ee_id),
      eeGoal: asString(row.ee_goal),
      eeTemplateName: asString(row.ee_template_name),
      turnOpen: row.turn_open === true,
      turnState: asTurnState(row.turn_state, row.turn_open === true),
      providerInterfaceId: asString(row.provider_interface_id),
      providerBoundInterfaceId: asString(row.provider_bound_interface_id),
      providerModel: asString(row.provider_model),
      providerModelsRefreshable: row.provider_models_refreshable === true,
      reasoningEffort: asString(row.reasoning_effort),
      serviceTier: asString(row.service_tier),
      workerInterfaceId: asString(row.worker_interface_id),
      workerChoiceInterfaceId: asString(row.worker_choice_interface_id),
      doorHostSlots: asDoorHostSlots(row.door_host_slots),
      createdAtMs: asNumber(row.created_at_ms),
      updatedAtMs: asNumber(row.updated_at_ms),
      lastMovementSeq: asNumber(row.last_movement_seq),
      nextScheduledAtMs: asNumber(row.next_scheduled_at_ms),
      messageCount: asNumber(row.message_count),
      pinnedArtifactIds: (Array.isArray(row.pinned_artifact_ids) ? row.pinned_artifact_ids : [])
        .map(asString)
        .filter(Boolean),
      pinnedContextItems: (Array.isArray(row.pinned_context_items) ? row.pinned_context_items : [])
        .map(asPinnedContextItem)
        .filter((item): item is ChatPinnedContextItem => item !== null),
      continuedFrom: asContinuation(row.continued_from),
      recordDigest: asString(row.record_digest),
      connectedApplicationIds: asStringArray(row.connected_application_ids),
      lastTerminalMessage: asTerminalMessage(row.last_terminal_message),
    }))
    .filter((run) => Boolean(run.runId))
    .sort((a, b) => b.updatedAtMs - a.updatedAtMs);
}

export async function loadChatRunStatuses(runIds: string[]): Promise<ChatRunStatus[]> {
  const requested = [...new Set(runIds.map((runId) => runId.trim()).filter(Boolean))];
  if (requested.length === 0) return [];
  const response = await readProjection<{ chats?: RawChatRunRow[] }>(
    "projection.operator.chats.status",
    { run_ids: requested },
  );
  const rows = Array.isArray(response?.chats) ? response.chats : [];
  return rows.flatMap((row) => {
    const runId = asString(row.run_id);
    if (!runId) return [];
    const turnOpen = row.turn_open === true;
    return [{
      runId,
      archived: row.archived === true,
      turnOpen,
      turnState: asTurnState(row.turn_state, turnOpen),
      updatedAtMs: asNumber(row.updated_at_ms),
      lastMovementSeq: asNumber(row.last_movement_seq),
    }];
  });
}

function asTerminalMessage(value: unknown): ChatTerminalMessage | null {
  const row = asRecord(value);
  const artifactId = asString(row.artifact_id);
  const interfaceId = asString(row.interface_id);
  const movementSeq = asNumber(row.movement_seq);
  if (!artifactId || !interfaceId || !movementSeq) return null;
  return {
    artifactId,
    createdAtMs: asNumber(row.created_at_ms),
    interfaceId,
    movementSeq,
    text: asString(row.text),
  };
}

export async function loadChatExecutionContexts(
  displayNames: ReadonlyMap<string, string> = new Map(),
  presentationColors: ReadonlyMap<string, string> = new Map(),
  presentationIcons: ReadonlyMap<string, string> = new Map(),
): Promise<ChatExecutionContext[]> {
  const response = await readProjection<{ application_catalog?: unknown[]; contexts?: unknown[] }>("projection.operator.chat.contexts");
  return projectChatExecutionContexts(response, displayNames, presentationColors, presentationIcons);
}

export function projectChatExecutionContexts(
  response: { application_catalog?: unknown[]; contexts?: unknown[] },
  displayNames: ReadonlyMap<string, string> = new Map(),
  presentationColors: ReadonlyMap<string, string> = new Map(),
  presentationIcons: ReadonlyMap<string, string> = new Map(),
): ChatExecutionContext[] {
  const applicationCatalog = new Map((Array.isArray(response.application_catalog) ? response.application_catalog : []).flatMap((value) => {
    const application = asRecord(value);
    const id = asString(application.id);
    return id ? [[id, application] as const] : [];
  }));
  return (Array.isArray(response.contexts) ? response.contexts : []).flatMap((value) => {
    const row = asRecord(value);
    const eeId = asString(row.ee_id);
    if (!eeId) return [];
    const workers = (Array.isArray(row.workers) ? row.workers : []).flatMap((workerValue) => {
      const worker = asRecord(workerValue);
      const id = asString(worker.id);
      if (!id) return [];
      return [{
        id,
        label: asString(worker.label) || id,
        authoringProfile: worker.authoring_profile === true,
        agentSourceTemplate: worker.agent_source_template === true,
        clientAgentId: asString(worker.client_agent_id),
        clientAgentName: asString(worker.client_agent_name),
        clientAgentIcon: asString(worker.client_agent_icon),
        clientAgentCreatedAtMs: asNumber(worker.client_agent_created_at_ms),
        clientAgentUpdatedAtMs: asNumber(worker.client_agent_updated_at_ms),
        clientAgentDeletedAtMs: asNumber(worker.client_agent_deleted_at_ms),
        clientTemplate: worker.client_template === true,
        clientTemplateProviderInterfaceId: asString(worker.client_template_provider_interface_id),
        clientTemplateProviderModel: asString(worker.client_template_provider_model),
        clientTemplateReasoningEffort: asString(worker.client_template_reasoning_effort),
        clientTemplateServiceTier: asString(worker.client_template_service_tier),
        clientTemplateDigest: asString(worker.client_template_digest),
        clientTemplateApplicationIds: asStringArray(worker.client_template_application_ids),
        clientTemplateWorkerEnvironmentKinds: asStringArray(worker.client_template_worker_environment_kinds),
        providerInterfaceIds: (Array.isArray(worker.provider_interface_ids) ? worker.provider_interface_ids : []).map(asString).filter(Boolean),
        interfacePolicy: asInterfacePolicy(worker.interface_policy),
        roleArtifactIds: (Array.isArray(worker.role_artifact_ids) ? worker.role_artifact_ids : []).map(asString).filter(Boolean),
        authoringRoomInterfaceId: asString(worker.authoring_room_interface_id),
        doorHostSlots: asDoorHostSlots(worker.door_host_slots),
        externalInputs: asExternalInputs(worker.external_inputs),
        externalRelays: asExternalRelays(worker.external_relays),
        modelOutputs: asModelOutputs(worker.model_outputs),
        contextArtifactBindings: asArtifactBindingIds(worker.context_artifact_bindings),
        payloadArtifactBindings: asPayloadArtifactBindings(worker.payload_artifact_bindings),
        selectedWorkerEnvironmentKinds: asStringArray(worker.selected_worker_environment_kinds),
        selectedTools: asApplicationToolCapabilities(worker.selected_tools),
        applications: asApplicationChoices(worker.applications, applicationCatalog),
        unavailableApplications: asUnavailableApplicationChoices(worker.unavailable_applications, applicationCatalog),
        drivers: asDriverChoices(worker.drivers),
      }];
    });
    if (!workers.length) return [];
    return [{
      eeId,
      agentCatalogDigest: asString(row.agent_catalog_digest),
      defaultTaskAccessMode: normalizeTaskAccessMode(row.default_task_access_mode),
      sourceFolders: asStringArray(row.source_folders),
      fullAccessRoot: asString(row.full_access_root),
      hostRootRefs: Object.fromEntries(Object.entries(asRecord(row.host_root_refs)).flatMap(([slot, root]) => (
        typeof root === "string" && root ? [[slot, root]] : []
      ))),
      isOsRoot: row.is_os_root === true,
      presentationColor: presentationColors.get(eeId) || "",
      presentationIcon: presentationIcons.get(eeId) || "",
      displayName: displayNames.get(eeId) || asString(row.display_name),
      goal: asString(row.goal),
      templateName: asString(row.template_name),
      parentEeId: asString(row.parent_ee_id),
      workers,
    }];
  });
}

export async function loadChatWorkerEnvironmentRooms(): Promise<ChatWorkerEnvironmentRoom[]> {
  const response = await readProjection<{ worker_environments?: unknown[] }>(
    "projection.worker_environments.list",
    { limit: 2_000 },
  );
  return (Array.isArray(response.worker_environments) ? response.worker_environments : []).flatMap((value) => {
    const row = asRecord(value);
    const workerEnvironmentId = asString(row.worker_environment_id);
    const doorInterfaceId = asString(row.door);
    const eeId = asString(row.ee_id);
    const runId = asString(row.run_id);
    if (!workerEnvironmentId || !doorInterfaceId || !eeId || !runId) return [];
    return [{
      workerEnvironmentId,
      runId,
      eeId,
      workerInterfaceId: asString(row.worker),
      doorInterfaceId,
      kind: asString(row.worker_environment_kind) || asString(row.kind),
      live: row.live_available === true,
      status: asString(row.status),
      lifecycle: asString(row.lifecycle),
      root: asString(row.root),
      lastActivityMovementSeq: asNumber(row.last_activity_movement_seq),
    }];
  }).sort((left, right) => (
    Number(right.lifecycle === "active") - Number(left.lifecycle === "active")
    || right.lastActivityMovementSeq - left.lastActivityMovementSeq
    || left.workerEnvironmentId.localeCompare(right.workerEnvironmentId)
  ));
}

function asTurnState(value: unknown, open: boolean): ChatRun["turnState"] {
  return value === "idle" || value === "waiting" || value === "working" || value === "failed"
    ? value
    : open
      ? "waiting"
      : "idle";
}

function asExternalInputs(value: unknown): ChatExternalInput[] {
  return (Array.isArray(value) ? value : []).flatMap((candidate) => {
    const input = asRecord(candidate);
    const checkpointId = asString(input.checkpoint_id);
    const interfaceId = asString(input.interface_id);
    const artifactNames = (Array.isArray(input.artifact_names) ? input.artifact_names : []).map(asString).filter(Boolean);
    const requiredVerifiedArtifacts = asRequiredVerifiedArtifacts(input.required_verified_artifacts);
    return checkpointId && interfaceId
      ? [{ checkpointId, interfaceId, ...(artifactNames.length ? { artifactNames } : {}), requiredVerifiedArtifacts }]
      : [];
  });
}

function asExternalRelays(value: unknown): ChatExternalRelay[] {
  return (Array.isArray(value) ? value : []).flatMap((candidate) => {
    const relay = asRecord(candidate);
    const checkpointId = asString(relay.checkpoint_id);
    const interfaceId = asString(relay.interface_id);
    const outboundInterfaceIds = (Array.isArray(relay.outbound_interface_ids)
      ? relay.outbound_interface_ids
      : []).map(asString).filter(Boolean);
    const requiredVerifiedArtifacts = asRequiredVerifiedArtifacts(relay.required_verified_artifacts);
    return checkpointId && interfaceId && outboundInterfaceIds.length
      ? [{ checkpointId, interfaceId, outboundInterfaceIds, requiredVerifiedArtifacts }]
      : [];
  });
}

function asModelOutputs(value: unknown): ChatModelOutput[] {
  return (Array.isArray(value) ? value : []).flatMap((candidate) => {
    const output = asRecord(candidate);
    const checkpointId = asString(output.checkpoint_id);
    const interfaceId = asString(output.interface_id);
    const requiredVerifiedArtifact = asRequiredVerifiedArtifact(output.required_verified_artifact);
    return checkpointId && interfaceId && requiredVerifiedArtifact
      ? [{ checkpointId, interfaceId, requiredVerifiedArtifact }]
      : [];
  });
}

function asRequiredVerifiedArtifacts(value: unknown): ChatRequiredVerifiedArtifact[] {
  return (Array.isArray(value) ? value : []).flatMap((candidate) => {
    const requirement = asRequiredVerifiedArtifact(candidate);
    return requirement ? [requirement] : [];
  });
}

function asRequiredVerifiedArtifact(value: unknown): ChatRequiredVerifiedArtifact | null {
  const requirement = asRecord(value);
  const mark = asString(requirement.mark);
  const artifactLabel = asString(requirement.artifact_label);
  const verifier = asString(requirement.verifier);
  const params = asRecord(requirement.params);
  return mark && artifactLabel && verifier
    ? { mark, artifactLabel, verifier, params }
    : null;
}

function asArtifactBindingIds(value: unknown): Array<{ id: string }> {
  return (Array.isArray(value) ? value : []).flatMap((candidate) => {
    const id = asString(asRecord(candidate).id);
    return id ? [{ id }] : [];
  });
}

function asPayloadArtifactBindings(value: unknown): ChatWorkerChoice["payloadArtifactBindings"] {
  if (value === undefined) return [];
  if (!Array.isArray(value)) throw new Error("Worker template file bindings must be an array.");
  const ids = new Set<string>();
  return value.map((candidate) => {
    const ref = asRecord(candidate);
    const id = asString(ref.id);
    const content = ref.content;
    if (!id || ids.has(id) || !Array.isArray(content) || content.length !== 32
      || content.some((byte) => !Number.isInteger(byte) || byte < 0 || byte > 255)) {
      throw new Error("Worker template contains an invalid file reference.");
    }
    ids.add(id);
    return { id, content: [...content] };
  });
}

function asApplicationChoices(
  value: unknown,
  catalog: ReadonlyMap<string, Record<string, unknown>>,
): ChatApplicationChoice[] {
  return (Array.isArray(value) ? value : []).flatMap((candidate) => {
    const selection = asRecord(candidate);
    const id = asString(selection.id);
    const application = catalog.get(id) ?? selection;
    const category = application.category;
    if (!id || (category !== "connector" && category !== "plugin" && category !== "artifact")) return [];
    return [{
      id,
      name: asString(application.name) || id,
      description: asString(application.description),
      category,
      icon: asString(application.icon),
      logoDataUrl: asString(application.logo_data_url),
      logoDarkDataUrl: asString(application.logo_dark_data_url),
      interfaces: asStringArray(application.interfaces),
      artifactBindings: (Array.isArray(application.artifact_bindings)
        ? application.artifact_bindings
        : []).flatMap((bindingValue) => {
          const binding = asRecord(bindingValue);
          const artifactId = asString(binding.artifact_id);
          return artifactId ? [{ artifactId, label: asString(binding.label) }] : [];
        }),
      taskCapabilities: asApplicationTaskCapabilities(application.task_capabilities),
      selectedByDefault: selection.selected_by_default === true,
    }];
  });
}

function asUnavailableApplicationChoices(
  value: unknown,
  catalog: ReadonlyMap<string, Record<string, unknown>>,
): ChatUnavailableApplicationChoice[] {
  const reasons = new Map((Array.isArray(value) ? value : []).map((candidate) => [
    asString(asRecord(candidate).id),
    asString(asRecord(candidate).reason),
  ] as const));
  return asApplicationChoices(value, catalog).flatMap((application) => {
    const reason = reasons.get(application.id);
    return reason ? [{ ...application, reason }] : [];
  });
}

function asApplicationTaskCapabilities(value: unknown): ChatApplicationChoice["taskCapabilities"] {
  const capabilities = asRecord(value);
  return {
    tools: asApplicationToolCapabilities(capabilities.tools),
  };
}

function asApplicationToolCapabilities(value: unknown): ChatApplicationToolCapability[] {
  return (Array.isArray(value) ? value : []).flatMap((candidate) => {
    const tool = asRecord(candidate);
    const id = asString(tool.tool);
    const interfaceId = asString(tool.interface);
    const workerEnvironmentKind = asString(tool.worker_environment_kind);
    return id && interfaceId && workerEnvironmentKind
      ? [{ tool: id, interface: interfaceId, workerEnvironmentKind }]
      : [];
  });
}

function asDriverChoices(value: unknown): ChatDriverChoice[] {
  return (Array.isArray(value) ? value : []).flatMap((candidate) => {
    const driver = asRecord(candidate);
    const name = asString(driver.name);
    if (!name) return [];
    const slots = asRecord(driver.host_slots);
    return [{
      name,
      capabilities: asStringArray(driver.capabilities),
      rootSlots: asStringArray(driver.root_slots),
      toolIds: asStringArray(driver.tool_ids),
      doorInterfaces: asStringArray(driver.door_interfaces),
      tools: asApplicationToolCapabilities(driver.tools),
      hostSlots: Object.fromEntries(Object.entries(slots).flatMap(([slot, root]) => (
        typeof root === "string" && root ? [[slot, root]] : []
      ))),
      selectedByDefault: driver.selected_by_default === true,
    }];
  });
}

function asStringArray(value: unknown): string[] {
  return (Array.isArray(value) ? value : []).map(asString).filter(Boolean);
}

function asInterfacePolicy(value: unknown): Record<string, Record<string, unknown>> {
  const policy = asRecord(value);
  return Object.fromEntries(Object.entries(policy).flatMap(([interfaceId, values]) => {
    const formed = asRecord(values);
    return interfaceId && Object.keys(formed).length ? [[interfaceId, formed]] : [];
  }));
}

function asPinnedContextItem(value: unknown): ChatPinnedContextItem | null {
  const row = asRecord(value);
  const artifactId = asString(row.artifact_id);
  const sourceArtifactId = asString(row.source_artifact_id);
  const sourceMessageId = asString(row.source_message_id);
  const text = asString(row.text);
  if (!artifactId || !sourceArtifactId || !sourceMessageId || !text) return null;
  const role = asRole(row.role);
  return {
    artifactId,
    sourceArtifactId,
    sourceMessageId,
    text,
    start: asNumber(row.start),
    end: asNumber(row.end),
    role,
  };
}

function asContinuation(value: unknown): ChatRun["continuedFrom"] {
  const row = asRecord(value);
  const runId = asString(row.run_id);
  const artifactId = asString(row.artifact_id);
  if (!runId || !artifactId) return null;
  return { runId, artifactId };
}

export async function loadChatRunMessages(runId: string): Promise<ChatRunMessage[]> {
  if (!runId) return [];
  const response = await readProjection<{ messages?: RawChatMessageRow[] }>(
    "projection.operator.chat.messages",
    { run_id: runId },
  );
  const rows = Array.isArray(response?.messages) ? response.messages : [];
  return chatRunMessages(rows, runId);
}

export async function searchChatTaskMessages(
  query: string,
  taskRunIds: string[],
  options: { beforeSeq?: number; limit?: number } = {},
): Promise<ChatTaskMessageSearchPage> {
  const normalizedQuery = query.trim();
  const normalizedTaskRunIds = [...new Set(taskRunIds.map((runId) => runId.trim()).filter(Boolean))];
  if (normalizedQuery.length < 2 || normalizedTaskRunIds.length === 0) {
    return {
      matches: [],
      hasOlder: false,
      nextBeforeSeq: -1,
      indexWatermark: -1,
      scope: {
        kind: "octopus_os_task_transcripts",
        taskCount: normalizedTaskRunIds.length,
        runCount: normalizedTaskRunIds.length,
        messageClasses: ["operator", "terminal_agent", "terminal_error"],
      },
    };
  }
  const response = await readProjection<{
    matches?: Array<{
      task_run_id?: unknown;
      source_run_id?: unknown;
      snippet?: unknown;
      message?: RawChatMessageRow;
    }>;
    has_older?: unknown;
    next_before_seq?: unknown;
    index_watermark?: unknown;
    scope?: {
      kind?: unknown;
      task_count?: unknown;
      run_count?: unknown;
      message_classes?: unknown;
    };
  }>("projection.operator.chat.search", {
    query: normalizedQuery,
    task_run_ids: normalizedTaskRunIds,
    ...(options.beforeSeq === undefined ? {} : { before_seq: options.beforeSeq }),
    limit: options.limit ?? 12,
  });
  const matches = (Array.isArray(response.matches) ? response.matches : []).flatMap((candidate) => {
    const taskRunId = asString(candidate.task_run_id);
    const sourceRunId = asString(candidate.source_run_id);
    const message = candidate.message
      ? chatRunMessages([candidate.message], sourceRunId)[0]
      : undefined;
    return taskRunId && sourceRunId && message
      ? [{ taskRunId, sourceRunId, snippet: asString(candidate.snippet), message }]
      : [];
  });
  const rawScope = response.scope ?? {};
  return {
    matches,
    hasOlder: response.has_older === true,
    nextBeforeSeq: asNumber(response.next_before_seq ?? -1),
    indexWatermark: asNumber(response.index_watermark ?? -1),
    scope: {
      kind: "octopus_os_task_transcripts",
      taskCount: asNumber(rawScope.task_count ?? normalizedTaskRunIds.length),
      runCount: asNumber(rawScope.run_count ?? normalizedTaskRunIds.length),
      messageClasses: asStringArray(rawScope.message_classes),
    },
  };
}

function chatRunMessages(rows: RawChatMessageRow[], runId: string): ChatRunMessage[] {
  return rows
    .map((row) => {
      const event = normalizeChatRunEventProjection(row.event_kind, row.event_data, row.text);
      return {
        id: asString(row.id) || asString(row.artifact_id),
        runId: asString(row.run_id) || runId,
        role: asRole(row.role),
        label: asString(row.label),
        direction: asDirection(row.direction),
        text: event.text,
        createdAtMs: asNumber(row.created_at_ms),
        movementSeq: asNumber(row.movement_seq),
        artifactId: asString(row.artifact_id),
        contentHash: asString(row.content_hash),
        interfaceId: asString(row.interface_id),
        eventKind: event.eventKind,
        eventData: event.eventData,
        latencyMs: asNumber(row.latency_ms),
        attachments: (Array.isArray(row.attachments) ? row.attachments : []).map((attachment: RawChatAttachmentRow) => ({
          id: asString(attachment.id),
          name: asString(attachment.name) || "Attachment",
          size: asNumber(attachment.size),
          type: asString(attachment.type) || "application/octet-stream",
          label: asString(attachment.label) || "operator.file",
          lastModified: asNumber(attachment.last_modified),
        })),
        error: row.error === true,
        terminal: row.terminal !== false,
      };
    })
    .filter((message) => Boolean(message.id))
    .sort((a, b) => a.createdAtMs - b.createdAtMs || a.movementSeq - b.movementSeq);
}

function asRecord(value: unknown): Record<string, unknown> {
  return value && typeof value === "object" && !Array.isArray(value)
    ? value as Record<string, unknown>
    : {};
}

function asDoorHostSlots(value: unknown): Record<string, Record<string, string>> {
  return Object.fromEntries(Object.entries(asRecord(value)).flatMap(([interfaceId, rawSlots]) => {
    const slots = Object.fromEntries(
      Object.entries(asRecord(rawSlots))
        .map(([slot, root]) => [slot, asString(root)])
        .filter(([, root]) => Boolean(root)),
    );
    return Object.keys(slots).length ? [[interfaceId, slots]] : [];
  }));
}

export function normalizeChatRunEventProjection(
  eventKindValue: unknown,
  eventDataValue: unknown,
  projectedTextValue: unknown,
  projection: "composed" | "fragment" = "composed",
): Pick<ChatRunMessage, "eventData" | "eventKind" | "text"> {
  const eventKind = asEventKind(eventKindValue);
  const eventData = asRecord(eventDataValue);
  const fragmentField = streamFragmentField(eventKind);
  const projectedText = asString(projectedTextValue);
  const fragment = fragmentField ? eventData[fragmentField] : undefined;
  const payloadProjection = asRecord(eventData._payload_projection);
  if (fragmentField && typeof fragment !== "string" && payloadProjection.truncated !== true) {
    return {
      eventKind: "generic_activity",
      eventData: {
        ...eventData,
        _projection_diagnostic: {
          declared_event_kind: eventKind,
          reason: `missing typed '${fragmentField}' fragment`,
        },
      },
      text: projectedText,
    };
  }
  return {
    eventKind,
    eventData,
    // Durable rows may already compose several fragments while retaining the
    // latest fragment in eventData. Live artifacts are individual movements,
    // so their typed fragment owns the visible copy (including exact empties).
    text: projection === "fragment" && typeof fragment === "string"
      ? fragment
      : projectedText,
  };
}

function streamFragmentField(eventKind: ChatRunEventKind): "delta" | "text" | null {
  if (eventKind === "message_delta" || eventKind === "commentary_delta") return "text";
  if (["reasoning_delta", "plan_delta", "tool_output_delta", "file_change_delta"].includes(eventKind)) return "delta";
  return null;
}

function asEventKind(value: unknown): ChatRunEventKind {
  const event = asString(value) as ChatRunEventKind;
  return [
    "message", "steering", "message_delta", "commentary", "commentary_delta", "reasoning", "reasoning_delta", "plan", "plan_delta", "usage",
    "tool", "tool_output_delta", "terminal_interaction", "file_change", "file_change_delta", "diff", "web_search",
    "hook", "hook_prompt", "mcp_tool", "mcp_progress", "mcp_status", "dynamic_tool", "collaboration",
    "subagent_activity", "image_view", "image_generation", "sleep", "review_entered", "review_exited", "compaction",
    "item_started", "turn_started", "turn_completed", "model_rerouted", "model_verification", "auth_recovery_started", "auth_recovery_completed", "warning",
    "auto_review_started", "auto_review_completed", "operator_request_resolved", "turn_control", "interrupted_result", "rate_limits", "goal", "approval",
    "elicitation", "user_input", "error", "generic_activity", "event", "input_recorded",
  ].includes(event)
    ? event
    : "generic_activity";
}

export { formatProjectionError };
