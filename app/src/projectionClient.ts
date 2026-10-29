// The client reads the projection — there is no "bridge" concept in the architecture.
// Two equal readers of the same engine (electron/projection.cjs):
//   - the desktop app serves it over IPC (window.octopusTransport),
//   - the vite dev server serves it over HTTP for the browser preview.
// Same methods, same rows, same truth.

import type {
  OctopusChatTurnRequest,
  OctopusChatTurnResponse,
  OctopusChatSteerRequest,
  OctopusChatSteerResponse,
  OctopusChatPinsRequest,
  OctopusChatPinsResponse,
  OctopusArtifactStreamAbortOutcome,
  OctopusArtifactStreamAbortRequest,
  OctopusArtifactStreamAppendRequest,
  OctopusArtifactStreamBeginRequest,
  OctopusArtifactStreamFinalizeRequest,
  OctopusArtifactStreamHandle,
  OctopusArtifactStreamOutcome,
  OctopusArtifactStreamProgress,
  OctopusInterfaceArtifactRequest,
  OctopusInterfaceArtifactResponse,
  OctopusDictationRequest,
  OctopusDictationResponse,
  OctopusProviderInterfacesResponse,
  OctopusProviderModelsRefreshRequest,
  OctopusProviderModelsRefreshResponse,
  OctopusProviderRateLimits,
  OctopusProviderRateLimitWindow,
  OctopusOperatorChatArtifactsRequest,
  OctopusOperatorChatArtifactsResponse,
  OctopusOperatorTurnControlRequest,
  OctopusOperatorTurnControlResponse,
  OctopusScheduledMutationOutcome,
  OctopusScheduledMutationRequest,
  OctopusOsWorkspaceMutationOutcome,
  OctopusOsWorkspaceMutationRequest,
  OctopusApplicationLifecycleProgress,
  OctopusApplicationPrivateConfigurationMutation,
  OctopusApplicationPrivateConfigurationOutcome,
  OctopusApplicationConnectionProbe,
  OctopusApplicationConnectionProgress,
  OctopusApplicationConnectionRequest,
  OctopusApplicationConnectionState,
  OctopusWorkspaceSetupProgress,
  OctopusWorkspaceSetupState,
  OctopusClientSetupBeginRequest,
  OctopusClientSetupProgress,
  OctopusClientSetupState,
} from "./electron";
import {
  PROJECTION_READ_CONTRACT_VERSION,
  type ProjectionReadMethod,
} from "./generated/projectionReadContract";

const DEV_PROJECTION_URL = "/__octopus/projection";
const DEV_PROVIDERS_URL = "/__octopus/providers";
const DEV_SUBMIT_URL = "/__octopus/submit";
const DEV_OPERATOR_CHAT_ARTIFACTS_URL = "/__octopus/operator-chat-artifacts";
const DEV_OPERATOR_TURN_CONTROL_URL = "/__octopus/operator-turn-control";
const DEV_STEER_URL = "/__octopus/chat-steer";
const DEV_CHAT_PINS_URL = "/__octopus/chat-pins";
const DEV_INTERFACE_ARTIFACT_URL = "/__octopus/interface-artifact";
const DEV_ARTIFACT_STREAM_URL = "/__octopus/artifact-stream";

export function hasProjectionBridge(): boolean {
  return Boolean(window.octopusTransport?.projectionRead) || import.meta.env.DEV;
}

export function projectionTargetLabel(): string {
  return "active client projection";
}

export async function readProjection<T = unknown, M extends ProjectionReadMethod = ProjectionReadMethod>(
  method: M,
  params?: Record<string, unknown>,
): Promise<T> {
  const request = {
    contract_version: PROJECTION_READ_CONTRACT_VERSION,
    method,
    ...(params === undefined ? {} : { params }),
  };
  const bridge = window.octopusTransport;
  if (bridge?.projectionRead) {
    return bridge.projectionRead(request) as Promise<T>;
  }
  if (import.meta.env.DEV) {
    const response = await fetch(DEV_PROJECTION_URL, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(request),
    });
    const payload = await response.json();
    if (!response.ok) {
      throw new Error(typeof payload?.error === "string" ? payload.error : `projection read failed: ${response.status}`);
    }
    return payload as T;
  }
  throw new Error("no projection reader is available: open the desktop app or the dev server");
}

export function canMutateScheduledInterfaces(): boolean {
  return Boolean(window.octopusTransport?.mutateScheduledInterface);
}

export async function mutateScheduledInterface(request: OctopusScheduledMutationRequest): Promise<OctopusScheduledMutationOutcome> {
  const host = window.octopusTransport;
  if (!host?.mutateScheduledInterface) throw new Error("Scheduled interface editing is available only in the live desktop client.");
  return host.mutateScheduledInterface(request);
}

export async function mutateApplicationPrivateConfiguration(
  request: OctopusApplicationPrivateConfigurationMutation,
): Promise<OctopusApplicationPrivateConfigurationOutcome> {
  const host = window.octopusTransport;
  if (!host?.mutateApplicationPrivateConfiguration) {
    throw new Error("Application configuration is available only in the live desktop client.");
  }
  return host.mutateApplicationPrivateConfiguration(request);
}

export function canConnectApplications(): boolean {
  return Boolean(window.octopusTransport?.applicationConnectionProbe);
}

export async function probeApplicationConnection(applicationId: string): Promise<OctopusApplicationConnectionProbe> {
  const host = window.octopusTransport;
  if (!host?.applicationConnectionProbe) throw new Error("Connecting applications is available only in the desktop client.");
  return host.applicationConnectionProbe({ applicationId });
}

export async function connectApplicationInterface(request: OctopusApplicationConnectionRequest): Promise<OctopusApplicationConnectionState> {
  const host = window.octopusTransport;
  if (!host?.applicationConnectionConnect) throw new Error("Connecting applications is available only in the desktop client.");
  return host.applicationConnectionConnect(request);
}

export async function cancelApplicationConnection(operationId: string): Promise<{ cancelled: boolean }> {
  const host = window.octopusTransport;
  if (!host?.applicationConnectionCancel) return { cancelled: false };
  return host.applicationConnectionCancel({ operationId });
}

export async function disconnectApplicationInterface(applicationId: string, interfaceId: string): Promise<OctopusApplicationConnectionState> {
  const host = window.octopusTransport;
  if (!host?.applicationConnectionDisconnect) throw new Error("Connecting applications is available only in the desktop client.");
  return host.applicationConnectionDisconnect({ applicationId, interfaceId });
}

export function onApplicationConnectionProgress(listener: (progress: OctopusApplicationConnectionProgress) => void): () => void {
  return window.octopusTransport?.onApplicationConnectionProgress?.(listener) ?? (() => {});
}

export async function mutateOctopusOsWorkspace(request: OctopusOsWorkspaceMutationRequest): Promise<OctopusOsWorkspaceMutationOutcome> {
  const host = window.octopusTransport;
  if (!host?.mutateOctopusOsWorkspace) throw new Error("Octopus OS workspace editing is available only in the live desktop client.");
  return host.mutateOctopusOsWorkspace(request);
}

export async function readWorkspaceSetupState(): Promise<OctopusWorkspaceSetupState> {
  const host = window.octopusTransport;
  if (!host?.workspaceSetupState) throw new Error("Workspace setup state is available only in the live desktop client.");
  return host.workspaceSetupState();
}

export async function readClientSetup(operationId: string): Promise<OctopusClientSetupState | null> {
  const host = window.octopusTransport;
  if (!host?.clientSetupRead) throw new Error("Client setup state is available only in the live desktop client.");
  return host.clientSetupRead({ operationId });
}

export async function readCurrentClientSetup(): Promise<OctopusClientSetupState | null> {
  const host = window.octopusTransport;
  if (!host?.clientSetupReadCurrent) throw new Error("Current client setup state is available only in the live desktop client.");
  return host.clientSetupReadCurrent();
}

export async function beginClientSetup(request: OctopusClientSetupBeginRequest): Promise<OctopusClientSetupState> {
  const host = window.octopusTransport;
  if (!host?.clientSetupBegin) throw new Error("Client setup is available only in the live desktop client.");
  return host.clientSetupBegin(request);
}

export async function resumeClientSetup(operationId: string, options: { allowSignIn?: boolean } = {}): Promise<OctopusClientSetupState> {
  const host = window.octopusTransport;
  if (!host?.clientSetupResume) throw new Error("Client setup is available only in the live desktop client.");
  return host.clientSetupResume({ operationId, ...(options.allowSignIn ? {} : { stopBefore: "provider" as const }) });
}

export function onApplicationLifecycleProgress(listener: (progress: OctopusApplicationLifecycleProgress) => void): () => void {
  return window.octopusTransport?.onApplicationLifecycleProgress?.(listener) ?? (() => {});
}

export function onWorkspaceSetupProgress(listener: (progress: OctopusWorkspaceSetupProgress) => void): () => void {
  return window.octopusTransport?.onWorkspaceSetupProgress?.(listener) ?? (() => {});
}

export function onClientSetupProgress(listener: (progress: OctopusClientSetupProgress) => void): () => void {
  return window.octopusTransport?.onClientSetupProgress?.(listener) ?? (() => {});
}

export async function readRuntimeProviderInterfaces(): Promise<OctopusProviderInterfacesResponse> {
  const bridge = window.octopusTransport;
  if (bridge?.providerInterfaces) {
    return bridge.providerInterfaces();
  }
  if (import.meta.env.DEV) {
    const httpResponse = await fetch(DEV_PROVIDERS_URL);
    const payload = await httpResponse.json();
    if (!httpResponse.ok) {
      throw new Error(typeof payload?.error === "string" ? payload.error : `provider read failed: ${httpResponse.status}`);
    }
    return payload as OctopusProviderInterfacesResponse;
  }
  return { repoRoot: null, providers: [], error: "no projection reader is available" };
}

export async function readProviderInterfaceRateLimits(): Promise<Map<string, OctopusProviderRateLimits>> {
  const status = await readProjection<{ providers?: unknown[] }>("projection.operator.providers.status");
  return new Map(
    (status.providers ?? []).flatMap((row) => {
      const normalized = normalizeProviderRateLimits(row);
      return normalized ? [[normalized.providerInterfaceId, normalized.rateLimits] as const] : [];
    }),
  );
}

export async function readProviderInterfaces(): Promise<OctopusProviderInterfacesResponse> {
  const response = await readRuntimeProviderInterfaces();
  if (response.error || response.providers.length === 0) return response;
  try {
    const byProvider = await readProviderInterfaceRateLimits();
    return {
      ...response,
      providers: response.providers.map((provider) => ({
        ...provider,
        rateLimits: byProvider.get(provider.id),
      })),
    };
  } catch (failure) {
    return {
      ...response,
      error: response.error ?? `Provider usage projection unavailable: ${failure instanceof Error ? failure.message : String(failure)}`,
    };
  }
}

function normalizeProviderRateLimits(value: unknown): { providerInterfaceId: string; rateLimits: OctopusProviderRateLimits } | null {
  if (!value || typeof value !== "object" || Array.isArray(value)) return null;
  const row = value as Record<string, unknown>;
  const providerInterfaceId = stringField(row.provider_interface_id);
  const snapshot = objectField(row.rate_limits);
  if (!providerInterfaceId || !snapshot) return null;
  return {
    providerInterfaceId,
    rateLimits: {
      observedAtMs: numberField(row.observed_at_ms),
      movementSeq: numberField(row.movement_seq),
      runId: stringField(row.run_id),
      limitId: stringField(snapshot.limitId ?? snapshot.limit_id),
      limitName: stringField(snapshot.limitName ?? snapshot.limit_name),
      primary: normalizeRateLimitWindow(snapshot.primary),
      secondary: normalizeRateLimitWindow(snapshot.secondary),
      credits: normalizeProviderCredits(snapshot.credits),
      individualLimit: normalizeIndividualLimit(snapshot.individualLimit ?? snapshot.individual_limit),
      planType: stringField(snapshot.planType ?? snapshot.plan_type),
      reachedType: stringField(snapshot.rateLimitReachedType ?? snapshot.rate_limit_reached_type),
    },
  };
}

function normalizeRateLimitWindow(value: unknown): OctopusProviderRateLimitWindow | null {
  const row = objectField(value);
  if (!row) return null;
  return {
    usedPercent: numberField(row.usedPercent ?? row.used_percent),
    windowDurationMins: nullableNumberField(row.windowDurationMins ?? row.window_duration_mins),
    resetsAt: nullableNumberField(row.resetsAt ?? row.resets_at),
  };
}

function normalizeProviderCredits(value: unknown): OctopusProviderRateLimits["credits"] {
  const row = objectField(value);
  if (!row) return null;
  return {
    hasCredits: row.hasCredits === true || row.has_credits === true,
    unlimited: row.unlimited === true,
    balance: nullableStringField(row.balance),
  };
}

function normalizeIndividualLimit(value: unknown): OctopusProviderRateLimits["individualLimit"] {
  const row = objectField(value);
  if (!row) return null;
  return {
    limit: stringField(row.limit),
    used: stringField(row.used),
    remainingPercent: numberField(row.remainingPercent ?? row.remaining_percent),
    resetsAt: numberField(row.resetsAt ?? row.resets_at),
  };
}

function objectField(value: unknown): Record<string, unknown> | null {
  return value && typeof value === "object" && !Array.isArray(value) ? value as Record<string, unknown> : null;
}

function stringField(value: unknown): string {
  return typeof value === "string" ? value : "";
}

function nullableStringField(value: unknown): string | null {
  return typeof value === "string" ? value : null;
}

function numberField(value: unknown): number {
  return typeof value === "number" && Number.isFinite(value) ? value : 0;
}

function nullableNumberField(value: unknown): number | null {
  return typeof value === "number" && Number.isFinite(value) ? value : null;
}

export function canSubmitOperatorTurn(): boolean {
  return Boolean(window.octopusTransport?.submitChatTurn) || import.meta.env.DEV;
}

export async function submitOperatorTurn(request: OctopusChatTurnRequest): Promise<OctopusChatTurnResponse> {
  const bridge = window.octopusTransport;
  if (bridge?.submitChatTurn) {
    return bridge.submitChatTurn(request);
  }
  if (import.meta.env.DEV) {
    if (request.attachments.length) throw new Error("Attachment submission requires the desktop attachment staging host.");
    const response = await fetch(DEV_SUBMIT_URL, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({
        endpoint: request.endpoint,
        timeoutMs: request.timeoutMs,
        messageId: request.messageId,
        recoverPreparedMessage: request.recoverPreparedMessage,
        runId: request.runId,
        providerInterfaceId: request.providerInterfaceId,
        providerModel: request.providerModel,
        reasoningEffort: request.reasoningEffort,
        serviceTier: request.serviceTier,
        eeId: request.eeId,
        workerInterfaceId: request.workerInterfaceId,
        doorHostSlots: request.doorHostSlots,
        externalInputs: request.externalInputs,
        externalRelays: request.externalRelays,
        modelOutputs: request.modelOutputs,
        contextArtifactBindings: request.contextArtifactBindings,
        selectedWorkerEnvironmentKinds: request.selectedWorkerEnvironmentKinds,
        selectedTools: request.selectedTools,
        clientSurfaceRole: request.clientSurfaceRole,
        clientContext: request.clientContext,
        continuation: request.continuation,
        text: request.text,
        attachments: [],
      }),
    });
    const payload = await response.json();
    if (!response.ok) {
      throw new Error(typeof payload?.error === "string" ? payload.error : `submit failed: ${response.status}`);
    }
    return payload as OctopusChatTurnResponse;
  }
  throw new Error("no local writer is available for the operator turn: open the desktop app or the dev server");
}

export async function readOperatorChatArtifacts(
  request: OctopusOperatorChatArtifactsRequest,
): Promise<OctopusOperatorChatArtifactsResponse> {
  const bridge = window.octopusTransport;
  if (bridge?.readOperatorChatArtifacts) {
    return bridge.readOperatorChatArtifacts(request);
  }
  if (import.meta.env.DEV) {
    const response = await fetch(DEV_OPERATOR_CHAT_ARTIFACTS_URL, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(request),
    });
    const payload = await response.json();
    if (!response.ok) {
      throw new Error(
        typeof payload?.error === "string"
          ? payload.error
          : `operator chat artifact read failed: ${response.status}`,
      );
    }
    return payload as OctopusOperatorChatArtifactsResponse;
  }
  throw new Error("no exact operator chat artifact reader is available");
}

export async function submitOperatorTurnControl(
  request: OctopusOperatorTurnControlRequest,
): Promise<OctopusOperatorTurnControlResponse> {
  if (window.octopusTransport?.submitOperatorTurnControl) {
    return window.octopusTransport.submitOperatorTurnControl(request);
  }
  if (!import.meta.env.DEV) return { deliveryError: { stage: "not-sent", message: "No local operator turn-control writer is available." } };
  const response = await fetch(DEV_OPERATOR_TURN_CONTROL_URL, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(request),
  });
  const payload = await response.json();
  if (!response.ok) {
    return { deliveryError: {
      stage: [401, 403, 405].includes(response.status) ? "not-sent" : "outcome-unknown",
      message: typeof payload?.error === "string" ? payload.error : `operator turn control failed: ${response.status}`,
    } };
  }
  return payload as OctopusOperatorTurnControlResponse;
}

export function canSubmitOperatorSteer(): boolean {
  return Boolean(window.octopusTransport?.submitChatSteer) || import.meta.env.DEV;
}

export async function submitOperatorSteer(request: OctopusChatSteerRequest): Promise<OctopusChatSteerResponse> {
  const host = window.octopusTransport;
  if (host?.submitChatSteer) return host.submitChatSteer(request);
  if (!import.meta.env.DEV) throw new Error("No local writer is available for an in-flight operator message.");
  if (request.attachments.length) throw new Error("Attachment submission requires the desktop attachment staging host.");
  const response = await fetch(DEV_STEER_URL, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({
      runId: request.runId,
      messageId: request.messageId,
      recoverPreparedMessage: request.recoverPreparedMessage,
      providerInterfaceId: request.providerInterfaceId,
      providerModel: request.providerModel,
      eeId: request.eeId,
      workerInterfaceId: request.workerInterfaceId,
      clientSurfaceRole: request.clientSurfaceRole,
      clientContext: request.clientContext,
      endpoint: request.endpoint,
      text: request.text,
      timeoutMs: request.timeoutMs,
      attachments: [],
    }),
  });
  const payload = await response.json();
  if (!response.ok) {
    throw new Error(typeof payload?.error === "string" ? payload.error : `steer failed: ${response.status}`);
  }
  return payload as OctopusChatSteerResponse;
}

export async function updateOperatorChatPins(request: OctopusChatPinsRequest): Promise<OctopusChatPinsResponse> {
  const bridge = window.octopusTransport;
  if (bridge?.updateChatPins) return bridge.updateChatPins(request);
  if (!import.meta.env.DEV) throw new Error("no local operator chat editor is available");
  const response = await fetch(DEV_CHAT_PINS_URL, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(request),
  });
  const payload = await response.json();
  if (!response.ok) {
    throw new Error(typeof payload?.error === "string" ? payload.error : `pin update failed: ${response.status}`);
  }
  return payload as OctopusChatPinsResponse;
}

export async function refreshProviderModels(
  request: OctopusProviderModelsRefreshRequest,
): Promise<OctopusProviderModelsRefreshResponse> {
  const host = window.octopusTransport;
  if (!host?.refreshProviderModels) {
    throw new Error("Provider model refresh is available only in the live desktop client.");
  }
  return host.refreshProviderModels(request);
}

export async function submitInterfaceArtifact(request: OctopusInterfaceArtifactRequest): Promise<OctopusInterfaceArtifactResponse> {
  if (window.octopusTransport?.submitInterfaceArtifact) {
    return window.octopusTransport.submitInterfaceArtifact(request);
  }
  if (!import.meta.env.DEV) throw new Error("No interface artifact writer is available.");
  const response = await fetch(DEV_INTERFACE_ARTIFACT_URL, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(request),
  });
  const payload = await response.json();
  if (!response.ok) {
    throw new Error(typeof payload?.error === "string" ? payload.error : `interface artifact submit failed: ${response.status}`);
  }
  return payload as OctopusInterfaceArtifactResponse;
}

export async function transcribeDictation(request: OctopusDictationRequest): Promise<OctopusDictationResponse> {
  const bridge = window.octopusTransport;
  if (!bridge?.transcribeDictation) throw new Error("Dictation requires the Octopus desktop interface host.");
  return bridge.transcribeDictation(request);
}

export async function beginInterfaceArtifactStream(request: OctopusArtifactStreamBeginRequest): Promise<OctopusArtifactStreamHandle> {
  if (window.octopusTransport?.beginInterfaceArtifactStream) return window.octopusTransport.beginInterfaceArtifactStream(request);
  return artifactStreamRequest<OctopusArtifactStreamHandle>("begin", request);
}

export async function appendInterfaceArtifactStream(request: OctopusArtifactStreamAppendRequest): Promise<OctopusArtifactStreamProgress> {
  if (window.octopusTransport?.appendInterfaceArtifactStream) return window.octopusTransport.appendInterfaceArtifactStream(request);
  return artifactStreamRequest<OctopusArtifactStreamProgress>("append", {
    streamId: request.streamId,
    base64: bytesToBase64(request.bytes),
  });
}

export async function finalizeInterfaceArtifactStream(request: OctopusArtifactStreamFinalizeRequest): Promise<OctopusArtifactStreamOutcome> {
  if (window.octopusTransport?.finalizeInterfaceArtifactStream) return window.octopusTransport.finalizeInterfaceArtifactStream(request);
  return artifactStreamRequest<OctopusArtifactStreamOutcome>("finalize", request);
}

export async function abortInterfaceArtifactStream(request: OctopusArtifactStreamAbortRequest): Promise<OctopusArtifactStreamAbortOutcome> {
  if (window.octopusTransport?.abortInterfaceArtifactStream) return window.octopusTransport.abortInterfaceArtifactStream(request);
  return artifactStreamRequest<OctopusArtifactStreamAbortOutcome>("abort", request);
}

async function artifactStreamRequest<T>(operation: "begin" | "append" | "finalize" | "abort", request: unknown): Promise<T> {
  if (!import.meta.env.DEV) throw new Error("No interface artifact stream transport is available.");
  const response = await fetch(`${DEV_ARTIFACT_STREAM_URL}/${operation}`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(request),
  });
  const payload = await response.json();
  if (!response.ok) throw new Error(typeof payload?.error === "string" ? payload.error : `artifact stream ${operation} failed: ${response.status}`);
  return payload as T;
}


function bytesToBase64(bytes: ArrayBuffer | Uint8Array): string {
  const view = bytes instanceof Uint8Array ? bytes : new Uint8Array(bytes);
  let binary = "";
  const CHUNK = 0x8000;
  for (let index = 0; index < view.length; index += CHUNK) {
    binary += String.fromCharCode(...view.subarray(index, index + CHUNK));
  }
  return btoa(binary);
}

export function formatProjectionError(error: unknown): string {
  if (error instanceof Error && error.message) return error.message;
  return String(error);
}
