import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState, type FormEvent, type SetStateAction } from "react";
import {
  formatProjectionError,
  loadChatRunMessages,
  loadChatRunStatuses,
  loadChatRuns,
  type ChatRun,
  type ChatRunStatus,
  type ChatRunMessage,
  type ChatPinnedContextItem,
  type ChatWorkerChoice,
  type ChatWorkerEnvironmentRoom,
} from "../../chatRuns";
import { useClientPreferences } from "../../clientPreferences";
import { useRuntimeHomeHealth } from "../../runtimeHost";
import { latestContextUsage } from "./contextUsage";
import { withRunOperationTargets } from "./runOperationTargets";
import {
  resolveClientAgents,
  visibleClientAgents,
  type ClientAgentDefinition,
} from "../../agentPresets";
import { reportClientExtensionIncident } from "../../extensions/incidents";
import { appToast } from "../../toasts";
import type {
  OctopusClientAccessProfile,
  OctopusClientContext,
  OctopusOperatorChatArtifact,
  OctopusOperatorChatMovementArtifact,
} from "../../electron";
import { DEFAULT_RUNTIME_ENDPOINT } from "../../gateway";
import {
  canSubmitOperatorSteer,
  canSubmitOperatorTurn,
  hasProjectionBridge,
  mutateOctopusOsWorkspace,
  submitInterfaceArtifact,
  submitOperatorSteer,
  submitOperatorTurn,
  submitOperatorTurnControl,
  updateOperatorChatPins,
} from "../../projectionClient";
import { useProjectionChange, useProjectionStructureGeneration } from "../../projectionLive";
import { createLatestProjectionRead } from "../../shell/latestProjectionRead";
import { readClientChatProjects, saveClientChatProjects } from "../../shell/clientWorkspaces";
import {
  formatRuntimeError,
  loadRuntimePrefs,
  RUNTIME_PREFS_CHANGED_EVENT,
  saveRuntimePrefs,
} from "../../runtimeClient";
import { operatorPermissionAnswer } from "./operator-permissions";
import { attachmentLabel, fileToBridgeAttachment, randomId } from "./artifacts";
import {
  eventNumber,
  eventRecord,
  eventString,
  liveArtifactMessage,
  liveToolCallOriginIndex,
  mergeChatRunMessages,
  mergeProjectedChatRuns,
  mergeRenderedMessages,
  toRender,
} from "./chatActivityProjection";
import {
  chatSettingsChangedEvent,
  loadChatSettings,
  saveChatSettings,
} from "./storage";
import type {
  ChatAttachment,
  ChatOperatorResponse,
  ChatQueuedTurnView,
  ChatRenderMessage,
} from "./types";
import type { ReviewTaskSeed } from "./reviewTask";
import {
  defaultCapabilitySelection,
  executionContextAllowsProvider,
  executionContextTaskCapabilities,
  providerDefaultModel,
  providerDefaultReasoningEffort,
  providerForExecutionContext,
  providerForExistingRun,
  providerInExecutionContext,
  providerModels,
  providerReasoningEfforts,
  providerSelectedModel,
  providerServiceTierAvailable,
  providerSelectedServiceTier,
  resolveNewTaskDefaults,
  taskCapabilitySelectionFailure,
  taskFullAccessSelectionFailure,
} from "./chatWorkspaceCapabilities";
import {
  chatObservationFailureMessage,
  observeChatArtifacts,
  observeChatReply,
  type ContinuationDraft,
} from "./chatTurnObservation";
import {
  operatorTurnResumeAcknowledgedOpen,
  operatorTurnControlReceipt,
  isChatReplyTerminal,
  reconcileChatRunReceipts,
  operatorTurnResumeAvailable,
} from "./chatTurnControl";

import { useChatCapabilityCatalog } from "./useChatCapabilityCatalog";
import { useTaskTitle } from "./useTaskTitle";
import { chatDraftKey, useChatDraftState, type ChatDraftValue } from "./useChatDraftState";
import { pendingQueueSteering, queueDispatchCandidates, queueSubmissionFailureStatus, queueTurnReady, visibleQueueTurns } from "./clientTurnQueue";
import { useChatTurnQueue, type QueuedChatTurn } from "./useChatTurnQueue";
import {
  taskAccessModeFromDoorHostSlots,
  type TaskAccessMode,
} from "./taskAccess";

function movementArtifact(
  artifact: OctopusOperatorChatArtifact,
): artifact is OctopusOperatorChatMovementArtifact {
  return artifact.source === "movement";
}

type ChatTurnSubmission = {
  queueId?: string;
  manual?: boolean;
  messageId: string;
  recoverPreparedMessage: boolean;
  replacesMessage?: { id: string; content: string };
  runId: string;
  runKey: string;
  text: string;
  attachments: ChatAttachment[];
  eeId: string;
  workerInterfaceId: string;
  providerInterfaceId: string;
  providerModel: string;
  reasoningEffort: string;
  serviceTier?: string;
  applicationIds: string[];
  driverNames: string[];
  accessMode: TaskAccessMode;
  endpoint: string;
  continuation: ContinuationDraft | null;
  clientSurfaceRole: "home" | "task";
  agentPresetId: string;
};

type PendingSteeringMessage = {
  runKey: string;
  message: ChatRenderMessage;
};

type PendingPausedQueueSubmission = {
  queuedTurnIds: string[];
  submission: ChatTurnSubmission;
};

type TurnControlTarget = {
  interfaceId: string;
  runId: string;
};

type PendingTurnControls = Record<string, { action: "resume" | "stop"; controlId: string; afterMovementSeq: number; requestPending?: boolean }>;

export function useChatWorkspace(
  accessProfile: OctopusClientAccessProfile,
  enabled = true,
  runtimeReady = true,
  storageScope = "main",
  clientSurfaceRole: "home" | "task" = "task",
  runtimeHome = "",
  clientContext: OctopusClientContext = [],
) {
  const { storageHealth, interfaceFailures } = useRuntimeHomeHealth(runtimeHome, accessProfile === "live");
  const storageBlockReason = storageHealth && storageHealth.status !== "writable" ? storageHealth.message || "Home storage is unavailable. Sending is paused; your draft is retained." : "";
  const clientContextRef = useRef(clientContext);
  clientContextRef.current = clientContext;
  const clientPreferences = useClientPreferences();
  const {
    executionContextError,
    executionContexts,
    executionContextsLoaded,
    providerInterfaceError,
    providerInterfaces,
    providerInterfacesLoaded,
    providerInterfacesRefreshing,
    providerUsageError,
    refreshProviderInterfaces,
    refreshProviderUsage,
    toolPresentations,
    workerEnvironmentRooms,
  } = useChatCapabilityCatalog(accessProfile, enabled, runtimeReady, runtimeHome);
  const {
    queueStore,
    queueOwner,
    queueReady,
    queueError,
    nextQueuedTurn,
    pausedQueueRunKeys,
    pausedQueueRunKeysRef,
    queuedTurns,
    queuedTurnsRef,
    replacePausedQueueRunKeys,
    replaceQueuedTurns,
  } = useChatTurnQueue(accessProfile, runtimeHome);
  const activeRunIdRef = useRef("");
  const deliveryContextRef = useRef({ accessProfile, runtimeHome, enabled, runtimeReady, storageBlockReason, interfaceFailures });
  deliveryContextRef.current = { accessProfile, runtimeHome, enabled, runtimeReady, storageBlockReason, interfaceFailures };
  function exactInterfaceFailureReason(interfaceId: string | undefined) {
    const failure = deliveryContextRef.current.interfaceFailures.find((item) => item.interfaceId === interfaceId);
    return failure ? `The provider for this task is unavailable (${failure.interfaceId}): ${failure.message}. Sending is paused; your draft is retained.` : "";
  }
  function interfaceFailureReason(runId: string, providerId = "") {
    return exactInterfaceFailureReason(runId ? projectedRunsRef.current.find((run) => run.runId === runId)?.providerBoundInterfaceId : providerId);
  }
  function deliveryScopeCurrent() {
    const latest = deliveryContextRef.current;
    return latest.accessProfile === accessProfile && latest.runtimeHome === runtimeHome;
  }
  function deliveryStillAllowed(owner: string, runId: string, background = false, providerId = "") {
    const latest = deliveryContextRef.current;
    return queueOwner.active && queueOwner.ready && queueOwner.id === owner && latest.enabled && latest.runtimeReady && !latest.storageBlockReason
      && !interfaceFailureReason(runId, providerId)
      && latest.accessProfile === accessProfile && latest.runtimeHome === runtimeHome && (background || activeRunIdRef.current === runId);
  }

  const projectedRunsRef = useRef<ChatRun[]>([]);
  const runReadGenerationRef = useRef(0);
  const runStatusReadGenerationRef = useRef(0);
  const runReceiptSequenceRef = useRef(new Map<string, number>());
  const messageReadGenerationRef = useRef(new Map<string, number>());
  const messageReadEpochRef = useRef(0);
  const messageCacheRef = useRef(new Map<string, ChatRunMessage[]>());
  const submitLockRef = useMemo(() => ({ current: new Set<string>() }), [accessProfile, runtimeHome]);
  const queueAttemptedProjection = useMemo(() => new Map<string, string>(), [accessProfile, runtimeHome]);
  const composerActionRef = useMemo(() => ({ current: new Set<string>() }), [accessProfile, runtimeHome]);
  const steeringComposerRef = useRef(new Set<string>());
  const pendingNewTaskTargetRef = useRef<{ eeId: string; agentPresetId: string } | null>(null);
  const runReads = useMemo(() => createLatestProjectionRead<ChatRun[]>(), [accessProfile, enabled, runtimeHome]);
  const statusReads = useMemo(() => createLatestProjectionRead<ChatRunStatus[]>(), [accessProfile, enabled, runtimeHome]);
  useEffect(() => () => runReads.invalidate(), [runReads]);
  useEffect(() => () => statusReads.invalidate(), [statusReads]);
  useEffect(() => () => {
    messageReadEpochRef.current += 1;
    messageReadGenerationRef.current.clear();
  }, [accessProfile, enabled, runtimeHome]);

  const [runs, setRuns] = useState<ChatRun[]>([]);
  const [runsError, setRunsError] = useState("");
  const [loadingRuns, setLoadingRuns] = useState(false);
  const [runsLoaded, setRunsLoaded] = useState(false);
  const [activeRunId, setActiveRunId] = useState("");
  const projectedActiveRun = useMemo(() => runs.find((run) => run.runId === activeRunId) ?? null, [activeRunId, runs]);
  const [error, setError] = useState("");
  const {
    agentPresetId,
    accessMode,
    applicationIds,
    applyDraft,
    attachments,
    clearSubmissionIntent,
    completeSubmissionIntent,
    consumeDraft,
    continuation,
    deleteDraft,
    dragOver,
    driverNames,
    dropFiles,
    eeId,
    fileInputRef,
    migrateDraft,
    messageId: draftMessageId,
    messageHadAttachments: draftMessageHadAttachments,
    navigateInputHistory,
    onDragLeave,
    onDragOver,
    onFilesPicked,
    providerInterfaceId,
    providerModel: draftProviderModel,
    readDraft,
    reasoningEffort: draftReasoningEffort,
    serviceTier: draftServiceTier,
    removeAttachment,
    resetInputHistory,
    saveCurrentDraft,
    setApplicationIds,
    setAgentPresetId,
    setAccessMode,
    setAttachments,
    setContinuation,
    setDriverNames,
    setEeId,
    setMessageId: setDraftMessageId,
    setProviderInterfaceId,
    setProviderModel,
    setReasoningEffort,
    setServiceTier,
    setRawText: setText,
    setText: setComposerText,
    setWorkerInterfaceId,
    text,
    workerInterfaceId,
    writeDraft,
  } = useChatDraftState({
    accessProfile,
    activeRunId,
    onError: setError,
    preserveSubmissionIntentOnEdit: (runId, messageId) => {
      const runKey = chatDraftKey(runId);
      return queuedTurnsRef.current.some((item) => (
        item.runKey === runKey
        && item.messageId === messageId
        && item.status !== "sending"
      ));
    },
    storageScope: runtimeHome ? JSON.stringify([runtimeHome, storageScope]) : "",
  });
  const providerModel = clientSurfaceRole === "home" ? projectedActiveRun?.providerModel ?? "" : draftProviderModel;
  const reasoningEffort = clientSurfaceRole === "home" ? projectedActiveRun?.reasoningEffort ?? "" : draftReasoningEffort;
  const serviceTier = clientSurfaceRole === "home" ? projectedActiveRun?.serviceTier ?? "" : draftServiceTier;
  const composerDraftRef = useRef({ runId: activeRunId, text, attachments });
  composerDraftRef.current = { runId: activeRunId, text, attachments };
  const initialDraft = readDraft("");
  const capabilitySelectionOwnerRef = useRef(initialDraft
    ? `${initialDraft.eeId}\0${initialDraft.workerInterfaceId}\0${initialDraft.agentPresetId}`
    : "");
  const [messages, setMessages] = useState<ChatRunMessage[]>([]);
  const [loadingMessages, setLoadingMessages] = useState(false);
  const [pendingByRunKey, setPendingByRunKey] = useState<Record<string, ChatRenderMessage[]>>({});
  const [pendingSteeringMessages, setPendingSteeringMessages] = useState<PendingSteeringMessage[]>([]);
  const [steeringComposerPendingRunKeys, setSteeringComposerPendingRunKeys] = useState<string[]>([]);
  const [pinningArtifactIds, setPinningArtifactIds] = useState<string[]>([]);

  const [endpoint, setEndpoint] = useState(() => (
    accessProfile === "live" ? loadRuntimePrefs().baseUrl : "lab://isolated"
  ));
  const [newTaskTargetResolutionBlocked, setNewTaskTargetResolutionBlocked] = useState(false);
  const [messageQuery, setMessageQuery] = useState("");
  const [inFlightRunKeys, setInFlightRunKeys] = useState<string[]>([]);
  const [inFlightTurnControlTargets, setInFlightTurnControlTargets] = useState<Record<string, TurnControlTarget>>({});
  const [pendingTurnControls, setPendingTurnControlsState] = useState<PendingTurnControls>({});
  const pendingTurnControlsRef = useRef<PendingTurnControls>({});
  const setPendingTurnControls = useCallback((update: SetStateAction<PendingTurnControls>) => {
    const next = typeof update === "function" ? update(pendingTurnControlsRef.current) : update;
    pendingTurnControlsRef.current = next;
    setPendingTurnControlsState(next);
  }, []);
  const [applicationMutationPending, setApplicationMutationPending] = useState("");
  const [pendingPausedQueueSubmission, setPendingPausedQueueSubmission] = useState<PendingPausedQueueSubmission | null>(null);

  useLayoutEffect(() => {
    runReadGenerationRef.current += 1; runStatusReadGenerationRef.current += 1;
    messageReadEpochRef.current += 1; messageReadGenerationRef.current.clear();
    messageCacheRef.current.clear(); runReceiptSequenceRef.current.clear(); projectedRunsRef.current = [];
    activeRunIdRef.current = "";
    setActiveRunId(""); setRuns([]); setMessages([]); setRunsLoaded(false);
    setPendingByRunKey({}); setPendingSteeringMessages([]); setInFlightRunKeys([]);
    setInFlightTurnControlTargets({}); setSteeringComposerPendingRunKeys([]); setPendingTurnControls({});
    setPendingPausedQueueSubmission(null); setError("");
  }, [accessProfile, runtimeHome]);

  const replaceRunPending = useCallback((runKey: string, nextMessages: ChatRenderMessage[]) => {
    setPendingByRunKey((current) => {
      if (nextMessages.length > 0) return { ...current, [runKey]: nextMessages };
      if (!(runKey in current)) return current;
      const next = { ...current };
      delete next[runKey];
      return next;
    });
  }, []);

  const mergeLiveRunMessages = useCallback((runId: string, incoming: ChatRunMessage[]) => {
    if (!incoming.length) return;
    const cached = messageCacheRef.current.get(runId) ?? [];
    const nextCached = mergeChatRunMessages(cached, incoming);
    messageCacheRef.current.set(runId, nextCached);
    if (activeRunIdRef.current === runId) {
      setMessages(nextCached);
    }
  }, []);

  const reconcileRunReceipt = useCallback((
    runId: string,
    artifacts: OctopusOperatorChatArtifact[],
  ) => {
    const floor = Math.max(
      runReceiptSequenceRef.current.get(runId) ?? -1,
      projectedRunsRef.current.find((run) => run.runId === runId)?.lastMovementSeq ?? -1,
    );
    const fresh = artifacts.filter(movementArtifact).filter((artifact) => artifact.run_id === runId && artifact.movement_seq > floor);
    if (!fresh.length) return false;
    runReceiptSequenceRef.current.set(runId, Math.max(...fresh.map((artifact) => artifact.movement_seq)));
    setRuns((current) => current.map((run) => run.runId === runId ? reconcileChatRunReceipts(run, fresh) : run));
    const latest = fresh.filter((artifact) => isChatReplyTerminal(artifact.event_kind, artifact.event_data, artifact.terminal))
      .reduce<OctopusOperatorChatMovementArtifact | null>((current, artifact) => !current || artifact.movement_seq > current.movement_seq ? artifact : current, null);
    if (!latest) return false;
    if (isChatReplyTerminal(latest.event_kind, latest.event_data, latest.terminal)) {
      setPendingTurnControls((current) => {
        if (!(runId in current)) return current;
        const next = { ...current };
        delete next[runId];
        return next;
      });
      setInFlightTurnControlTargets((current) => {
        if (!(runId in current)) return current;
        const next = { ...current };
        delete next[runId];
        return next;
      });
    }
    return true;
  }, []);

  useEffect(() => {
    const pending = pendingTurnControls[activeRunId];
    const run = runs.find((candidate) => candidate.runId === activeRunId);
    if (!pending || !run) return;
    const receipt = operatorTurnControlReceipt(run, messages, pending);
    if (!receipt) return;
    setPendingTurnControls((current) => {
      if (current[run.runId]?.controlId !== pending.controlId) return current;
      const next = { ...current };
      delete next[run.runId];
      return next;
    });
    if (receipt.eventData.status === "failed") {
      const detail = eventString(eventRecord(receipt.eventData.detail).error) || receipt.text;
      setError(`${pending.action === "stop" ? "Stop" : "Resume"} failed.${detail ? ` ${detail}` : ""}`);
    }
  }, [activeRunId, messages, pendingTurnControls, runs, setPendingTurnControls]);

  const acquireSubmission = useCallback((runKey: string) => {
    if (submitLockRef.current.has(runKey)) return false;
    submitLockRef.current.add(runKey);
    setInFlightRunKeys((current) => current.includes(runKey) ? current : [...current, runKey]);
    return true;
  }, []);

  const migrateSubmission = useCallback((fromRunKey: string, toRunKey: string) => {
    if (fromRunKey === toRunKey) return;
    if (!submitLockRef.current.delete(fromRunKey)) {
      throw new Error(`submission lock '${fromRunKey}' disappeared before run identity was authored`);
    }
    if (submitLockRef.current.has(toRunKey)) {
      submitLockRef.current.add(fromRunKey);
      throw new Error(`run '${toRunKey}' already has an in-flight operator turn`);
    }
    submitLockRef.current.add(toRunKey);
    setInFlightRunKeys((current) => [...new Set(current.filter((key) => key !== fromRunKey).concat(toRunKey))]);
    setPendingByRunKey((current) => {
      const next = { ...current };
      const migrating = next[fromRunKey];
      delete next[fromRunKey];
      if (migrating) next[toRunKey] = migrating;
      return next;
    });
  }, []);

  const releaseSubmission = useCallback((runKey: string) => {
    submitLockRef.current.delete(runKey);
    setInFlightRunKeys((current) => current.filter((key) => key !== runKey));
  }, []);

  useEffect(() => {
    if (messages.length === 0) return;
    const projectedIds = new Set(messages.map((message) => message.id));
    setPendingSteeringMessages((current) => current.filter((entry) => !projectedIds.has(entry.message.id)));
    setPendingTurnControls((current) => {
      const settledRunIds = Object.keys(current).filter((runId) => messages.some((message) => message.runId === runId && (
        message.eventKind === "turn_control"
          && eventString(message.eventData?.control_id) === current[runId].controlId
          && ["acknowledged", "failed"].includes(eventString(message.eventData?.status))
        || message.movementSeq > current[runId].afterMovementSeq
          && isChatReplyTerminal(message.eventKind, message.eventData, message.terminal)
      )));
      if (!settledRunIds.length) return current;
      const next = { ...current };
      for (const runId of settledRunIds) delete next[runId];
      return next;
    });
  }, [messages]);

  const refreshRuns = useCallback(async () => {
    if (!deliveryScopeCurrent()) return;
    const generation = runReadGenerationRef.current + 1;
    runReadGenerationRef.current = generation;
    runStatusReadGenerationRef.current += 1;
    statusReads.invalidate();
    if (!enabled || !hasProjectionBridge()) {
      setRunsLoaded(false);
      setLoadingRuns(false);
      return;
    }
    setLoadingRuns(true);
    try {
      const nextRuns = await runReads.request(loadChatRuns);
      if (!nextRuns || !deliveryScopeCurrent()) return;
      projectedRunsRef.current = nextRuns;
      setRuns((current) => mergeProjectedChatRuns(current, nextRuns));
      setRunsError("");
      setRunsLoaded(true);
    } catch (failure) {
      if (runReadGenerationRef.current === generation) {
        setRunsLoaded(false);
        setRunsError(formatProjectionError(failure));
      }
    } finally {
      if (runReadGenerationRef.current === generation) {
        setLoadingRuns(false);
      }
    }
  }, [accessProfile, enabled, runtimeHome, runReads, statusReads]);

  const refreshMessages = useCallback(async (runId: string) => {
    if (!deliveryScopeCurrent()) return null;
    const epoch = messageReadEpochRef.current;
    const generation = (messageReadGenerationRef.current.get(runId) ?? 0) + 1;
    messageReadGenerationRef.current.set(runId, generation);
    if (!enabled || !runId || !hasProjectionBridge()) {
      if (messageReadGenerationRef.current.get(runId) === generation && !activeRunIdRef.current) {
        setMessages([]);
        setLoadingMessages(false);
      }
      return null;
    }
    try {
      const nextMessages = await loadChatRunMessages(runId);
      if (messageReadEpochRef.current !== epoch || messageReadGenerationRef.current.get(runId) !== generation) return null;
      const merged = mergeChatRunMessages(messageCacheRef.current.get(runId) ?? [], nextMessages, "snapshot");
      messageCacheRef.current.set(runId, merged);
      if (
        messageReadGenerationRef.current.get(runId) === generation
        && activeRunIdRef.current === runId
      ) {
        setMessages(merged);
        setLoadingMessages(false);
      }
      return merged;
    } catch (failure) {
      if (messageReadEpochRef.current === epoch && messageReadGenerationRef.current.get(runId) === generation && activeRunIdRef.current === runId) {
        setRunsError(formatProjectionError(failure));
        setLoadingMessages(false);
      }
      return null;
    }
  }, [accessProfile, enabled, runtimeHome]);

  const refresh = useCallback(async () => {
    await refreshRuns();
    await refreshMessages(activeRunIdRef.current);
  }, [refreshRuns, refreshMessages]);

  const projectionChange = useProjectionChange();
  const projectionGeneration = projectionChange.generation;
  const projectionStructureGeneration = useProjectionStructureGeneration();

  useEffect(() => {
    void refreshRuns();
  }, [refreshRuns, projectionStructureGeneration]);

  useEffect(() => {
    if (activeRunId) void refreshRuns();
  }, [activeRunId, refreshRuns]);

  useEffect(() => {
    void refreshMessages(activeRunId);
  }, [activeRunId, refreshMessages, projectionStructureGeneration]);

  useEffect(() => {
    if (!enabled || !projectionGeneration || projectionChange.kind !== "ledger") return;
    if (accessProfile === "live") {
      const runIds = projectedRunsRef.current.map((run) => run.runId);
      if (runIds.length === 0) return;
      const generation = runStatusReadGenerationRef.current;
      void statusReads.request(() => loadChatRunStatuses(runIds)).then((statuses) => {
        if (!statuses || runStatusReadGenerationRef.current !== generation) return;
        const byRun = new Map(statuses.map((status) => [status.runId, status]));
        setRuns((current) => {
          let changed = false;
          const next = current.map((run) => {
            const status = byRun.get(run.runId);
            if (!status || status.lastMovementSeq < run.lastMovementSeq) return run;
            runReceiptSequenceRef.current.set(run.runId, Math.max(runReceiptSequenceRef.current.get(run.runId) ?? -1, status.lastMovementSeq));
            const updatedAtMs = Math.max(run.updatedAtMs, status.updatedAtMs);
            if (
              run.turnOpen === status.turnOpen
              && run.turnState === status.turnState
              && run.lastMovementSeq === status.lastMovementSeq
              && run.updatedAtMs === updatedAtMs
            ) return run;
            changed = true;
            return {
              ...run,
              turnOpen: status.turnOpen,
              turnState: status.turnState,
              updatedAtMs,
              lastMovementSeq: status.lastMovementSeq,
            };
          });
          return changed ? next : current;
        });
        setRunsError("");
      }).catch((failure) => {
        if (runStatusReadGenerationRef.current === generation) {
          setRunsError(formatProjectionError(failure));
        }
      });
      return;
    }
    void refreshRuns();
    if (activeRunId) void refreshMessages(activeRunId);
  }, [
    accessProfile,
    enabled,
    projectionChange.kind,
    projectionGeneration,
    activeRunId,
    refreshRuns,
    refreshMessages,
    statusReads,
  ]);

  useEffect(() => {
    if (accessProfile !== "live") return;
    saveRuntimePrefs({ baseUrl: endpoint });
  }, [accessProfile, endpoint]);

  useEffect(() => {
    if (accessProfile !== "live") return;
    function refreshPreference() {
      setEndpoint(loadRuntimePrefs().baseUrl);
    }
    window.addEventListener(RUNTIME_PREFS_CHANGED_EVENT, refreshPreference);
    return () => window.removeEventListener(RUNTIME_PREFS_CHANGED_EVENT, refreshPreference);
  }, [accessProfile]);

  useEffect(() => {
    function refreshPreference() {
      if (!activeRunIdRef.current) {
        const settings = loadChatSettings(accessProfile);
        const defaults = resolveNewTaskDefaults(runs, settings);
        setProviderInterfaceId(defaults?.providerInterfaceId ?? "");
        setProviderModel(defaults?.providerModel ?? "");
        setReasoningEffort(defaults?.reasoningEffort ?? "");
        setServiceTier(defaults?.serviceTier ?? "");
      }
    }
    const eventName = chatSettingsChangedEvent(accessProfile);
    window.addEventListener(eventName, refreshPreference);
    return () => window.removeEventListener(eventName, refreshPreference);
  }, [accessProfile, runs]);

  useEffect(() => {
    if (!enabled || accessProfile !== "live") return;
    let cancelled = false;
    window.octopusTransport?.defaults()
      .then((defaults) => {
        if (cancelled) return;
        if (!endpoint || endpoint === DEFAULT_RUNTIME_ENDPOINT) {
          setEndpoint(defaults.endpoint);
        }
      })
      .catch(() => undefined);
    return () => {
      cancelled = true;
    };
  }, [accessProfile, enabled, endpoint]);

  const inFlightTurnControlTarget = inFlightTurnControlTargets[activeRunId] ?? null;
  const activeRun = projectedActiveRun;
  useTaskTitle(activeRun, runtimeHome, accessProfile, enabled && runtimeReady);
  const activeTurnControlTarget = inFlightTurnControlTarget ?? (
    (projectedActiveRun?.turnOpen || operatorTurnResumeAcknowledgedOpen(messages, projectedActiveRun?.lastMovementSeq ?? -1))
      && projectedActiveRun?.chatInterfaceId
      ? { interfaceId: projectedActiveRun.chatInterfaceId, runId: projectedActiveRun.runId }
      : null
  );
  const turnOpen = Boolean(activeTurnControlTarget);
  const resumeAvailable = Boolean(
    activeRun?.writable
    && activeRun.chatInterfaceId
    && operatorTurnResumeAvailable(messages, turnOpen)
  );
  useEffect(() => {
    if (!activeRun || activeRun.runId !== activeRunIdRef.current) return;
    setEeId(activeRun.eeId);
    setWorkerInterfaceId(activeRun.workerChoiceInterfaceId || activeRun.workerInterfaceId);
    setProviderInterfaceId(activeRun.providerInterfaceId);
    setProviderModel((current) => current || activeRun.providerModel);
    setReasoningEffort((current) => current || activeRun.reasoningEffort);
    setServiceTier((current) => current || activeRun.serviceTier || "");
  }, [activeRun]);

  useEffect(() => {
    if (
      accessProfile !== "live"
      || !enabled
      || !activeRun?.writable
      || !activeRun.chatInterfaceId
    ) return;
    let cancelled = false;
    let observationError = "";
    const runId = activeRun.runId;
    const interfaceId = activeRun.chatInterfaceId;
    const toolCallOrigins = liveToolCallOriginIndex(messageCacheRef.current.get(runId) ?? []);
    const afterSeq = (messageCacheRef.current.get(runId) ?? [])
      .filter((message) => message.eventData._movement_evidence !== true)
      .reduce((latest, message) => Math.max(latest, message.movementSeq), -1);

    void observeChatArtifacts({
      interfaceId, runId, afterSeq,
      isCurrent: () => !cancelled,
      onArtifacts: (artifacts) => {
        const visible = artifacts.map((artifact) => {
          const projected = liveArtifactMessage(
            artifact,
            toolCallOrigins,
          );
          return projected;
        });
        mergeLiveRunMessages(runId, visible);
        const terminalReceived = reconcileRunReceipt(runId, artifacts);
        if (artifacts.some((artifact) => artifact.event_kind === "rate_limits")) {
          void refreshProviderUsage();
        }
        if (terminalReceived) {
          void refreshMessages(runId);
          void refreshRuns();
        }
      },
      onFailure: (failure) => {
        observationError = formatRuntimeError(failure);
        setError(observationError);
      },
      onRecovered: () => {
        const recovered = observationError;
        if (!recovered) return;
        observationError = "";
        setError((current) => current === recovered ? "" : current);
      },
    }).catch((failure) => {
      if (!cancelled) setError(formatRuntimeError(failure));
    });
    return () => {
      cancelled = true;
    };
  }, [
    accessProfile,
    activeRun?.chatInterfaceId,
    activeRun?.runId,
    activeRun?.writable,
    enabled,
    mergeLiveRunMessages,
    reconcileRunReceipt,
    refreshMessages,
    refreshProviderUsage,
    refreshRuns,
  ]);

  useEffect(() => {
    if (activeRunIdRef.current) return;
    if (!runsLoaded) return;
    const defaults = resolveNewTaskDefaults(runs, loadChatSettings(accessProfile));
    if (!defaults) return;
    if (!pendingNewTaskTargetRef.current && !eeId) {
      setEeId(defaults.eeId);
      setWorkerInterfaceId((current) => current || defaults.workerInterfaceId);
    }
    setProviderInterfaceId((current) => current || defaults.providerInterfaceId);
    setProviderModel((current) => current || defaults.providerModel);
    setReasoningEffort((current) => current || defaults.reasoningEffort);
    setServiceTier((current) => current || defaults.serviceTier || "");
  }, [accessProfile, clientSurfaceRole, eeId, runs, runsLoaded]);

  const activeRunKey = chatDraftKey(activeRunId);
  const inFlightRunKeySet = useMemo(() => new Set(inFlightRunKeys), [inFlightRunKeys]);
  const steeringComposerPendingRunKeySet = useMemo(
    () => new Set(steeringComposerPendingRunKeys),
    [steeringComposerPendingRunKeys],
  );
  const activeRunInFlight = inFlightRunKeySet.has(activeRunKey);
  const activeRunSteeringPending = steeringComposerPendingRunKeySet.has(activeRunKey);
  const replyOpen = activeRunInFlight || turnOpen;
  const replyRunKey = replyOpen ? activeRunKey : "";
  const applicationSelectionLocked = Boolean(activeRunId) && !(
    accessProfile === "live"
    && activeRun?.writable === true
    && Boolean(activeRun.recordDigest)
    && !replyOpen
    && !applicationMutationPending
  );
  const applicationSelectionLockReason: "" | "turn" | "saving" | "read-only" = !activeRunId || !applicationSelectionLocked
    ? ""
    : replyOpen
      ? "turn"
      : applicationMutationPending
        ? "saving"
        : "read-only";

  const selectedExecutionContext = useMemo(() => executionContexts.find((context) => context.eeId === eeId) ?? null, [eeId, executionContexts]);
  const availableWorkers = selectedExecutionContext?.workers ?? [];
  const selectedWorker = availableWorkers.find((worker) => worker.id === workerInterfaceId) ?? null;
  const clientAgentDefaults = useMemo(() => !runsLoaded
    ? null
    : resolveNewTaskDefaults(runs, loadChatSettings(accessProfile)), [
    accessProfile,
    providerInterfaceId,
    providerModel,
    reasoningEffort,
    serviceTier,
    runs,
    runsLoaded,
  ]);
  const newTaskDefaults = activeRunId ? null : clientAgentDefaults;
  const allAgentPresets = useMemo(() => resolveClientAgents(executionContexts), [executionContexts]);
  const agentPresets = useMemo(() => visibleClientAgents(allAgentPresets), [allAgentPresets]);
  const agentProviderInterfaces = useMemo(() => {
    const preferredProviderInterfaceId = clientAgentDefaults?.providerInterfaceId ?? "";
    if (!preferredProviderInterfaceId) return providerInterfaces;
    return providerInterfaces.map((provider) => ({
      ...provider,
      preferred: provider.id === preferredProviderInterfaceId,
    }));
  }, [clientAgentDefaults?.providerInterfaceId, providerInterfaces]);
  const selectedClientAgent = activeRunId
    ? null
    : agentPresets.find((agent) => agent.id === agentPresetId) ?? null;
  const activeTaskAgent = activeRun ? clientAgentForRun(allAgentPresets, activeRun) : null;
  const capabilitySelectionReady = Boolean(
    executionContextsLoaded && providerInterfacesLoaded
    && !pendingNewTaskTargetRef.current && !newTaskTargetResolutionBlocked
    && selectedClientAgent && selectedWorker
    && selectedClientAgent.projectEeId === eeId
    && selectedClientAgent.workerInterfaceId === workerInterfaceId
    && capabilitySelectionOwnerRef.current === `${eeId}\0${workerInterfaceId}\0${selectedClientAgent.id}`
  );
  const selectedClientAgentFailure = executionContextsLoaded && providerInterfacesLoaded
    && !pendingNewTaskTargetRef.current && selectedClientAgent
    && selectedClientAgent.projectEeId === eeId && selectedClientAgent.workerInterfaceId === workerInterfaceId
    ? clientAgentSelectionFailure(selectedClientAgent)
    : "";
  const selectedClientAgentCapabilityFailure = capabilitySelectionReady && selectedClientAgent
    ? clientAgentCapabilitySelectionFailure(
        selectedClientAgent,
        selectedWorker,
        applicationIds,
        driverNames,
        accessMode,
      )
    : "";
  const workspaceSelectionError = useMemo(() => {
    if (activeRunId || !capabilitySelectionReady || selectedClientAgentCapabilityFailure) return "";
    try {
      return executionContextTaskCapabilities(executionContexts, eeId, workerInterfaceId, applicationIds, driverNames, accessMode).workspaceSelectionError;
    } catch (failure) {
      return failure instanceof Error ? failure.message : String(failure);
    }
  }, [activeRunId, capabilitySelectionReady, selectedClientAgentCapabilityFailure, executionContexts, eeId, workerInterfaceId, applicationIds, driverNames, accessMode]);
  const clientAgentSelectionReady = Boolean(
    selectedClientAgent
    && !selectedClientAgentFailure
    && selectedClientAgent.projectEeId === eeId
    && selectedClientAgent.workerInterfaceId === workerInterfaceId
  );
  const fullAccessUnavailableReason = executionContextError || selectedClientAgentFailure || (!capabilitySelectionReady
    ? !executionContextsLoaded || !providerInterfacesLoaded || pendingNewTaskTargetRef.current
      ? "Wait for the selected Agent’s capabilities to load."
      : "Select an Agent with available capabilities before enabling full access."
    : selectedClientAgentCapabilityFailure || taskFullAccessSelectionFailure(
        selectedExecutionContext, selectedWorker, applicationIds, driverNames,
      ));
  useEffect(() => {
    if (
      !activeRun
      || !executionContextsLoaded
      || !providerInterfacesLoaded
    ) return;
    const inferredAgent = clientAgentForRun(allAgentPresets, activeRun);
    if (inferredAgent) {
      if (inferredAgent.id !== agentPresetId) setAgentPresetId(inferredAgent.id);
      return;
    }
    if (agentPresetId) setAgentPresetId("");
  }, [
    activeRun,
    agentPresetId,
    allAgentPresets,
    executionContextsLoaded,
    providerInterfacesLoaded,
    setAgentPresetId,
  ]);
  const newTaskAuthoringError = !activeRunId && runsLoaded && !newTaskDefaults
    ? "New task authoring requires exactly one writable Home projected in the Octopus OS root EE."
    : "";
  const resolvedProviderInterfaceId = activeRunId
    ? activeRun?.providerInterfaceId ?? ""
    : providerInterfaceId;
  const providerFailureReason = exactInterfaceFailureReason(activeRunId ? activeRun?.providerBoundInterfaceId : resolvedProviderInterfaceId);
  const runtimeBlockReason = [storageBlockReason, providerFailureReason].filter(Boolean).join("\n");
  const providerSelectionReady = providerInterfacesLoaded && Boolean(
    activeRunId ? activeRun && resolvedProviderInterfaceId : newTaskDefaults && resolvedProviderInterfaceId,
  );
  const selectableProviderInterfaces = useMemo(() => {
    const allowed = new Set(selectedWorker?.providerInterfaceIds ?? []);
    if (!selectedWorker) return [];
    return providerInterfaces
      .filter((provider) => allowed.has(provider.id))
      .map((provider) => providerForExecutionContext(provider, selectedWorker))
      .filter((provider) => provider.models.length > 0);
  }, [providerInterfaces, selectedWorker]);
  const declaredActiveProvider = useMemo(() => {
    if (!providerSelectionReady) return undefined;
    const provider = providerInterfaces.find((candidate) => candidate.id === resolvedProviderInterfaceId);
    return provider && selectedWorker ? providerForExecutionContext(provider, selectedWorker) : provider;
  }, [providerInterfaces, providerSelectionReady, resolvedProviderInterfaceId, selectedWorker]);
  const activeProvider = !providerSelectionReady
    ? undefined
    : activeRunId
      ? declaredActiveProvider
      : selectableProviderInterfaces.find((provider) => provider.id === providerInterfaceId);
  const modelSelectionLoading = enabled && (
    !providerInterfacesLoaded || providerInterfacesRefreshing
    || !executionContextsLoaded && !executionContextError
    || !runsLoaded && !runsError
  );
  const presentedProviderInterfaces = activeRunId && declaredActiveProvider
    && !selectableProviderInterfaces.some((provider) => provider.id === declaredActiveProvider.id)
    ? [declaredActiveProvider, ...selectableProviderInterfaces]
    : selectableProviderInterfaces;
  const refreshProviderModels = useCallback(async () => {
    if (accessProfile === "live" && enabled) await refreshProviderInterfaces();
  }, [accessProfile, enabled, refreshProviderInterfaces]);
  const executionContextReady = Boolean(
    activeRunId
    || executionContextsLoaded
      && pendingNewTaskTargetRef.current === null
      && !newTaskTargetResolutionBlocked
      && eeId
      && workerInterfaceId
      && clientAgentSelectionReady
      && capabilitySelectionReady
      && !selectedClientAgentCapabilityFailure
      && selectedExecutionContext?.workers.some((worker) => worker.id === workerInterfaceId),
  );

  useEffect(() => {
    if (activeRunId || !executionContextsLoaded || !selectedExecutionContext || !selectedWorker) return;
    if (!selectedClientAgent || selectedClientAgent.projectEeId !== selectedExecutionContext.eeId
      || selectedClientAgent.workerInterfaceId !== selectedWorker.id || pendingNewTaskTargetRef.current) return;
    const owner = `${selectedExecutionContext.eeId}\0${selectedWorker.id}\0${selectedClientAgent?.id ?? ""}`;
    const agentApplicationIds = new Set(selectedClientAgent?.applicationIds ?? []);
    const agentDriverNames = new Set(selectedClientAgent?.driverNames ?? []);
    const availableApplications = new Set(selectedWorker.applications
      .map((application) => application.id)
      .filter((id) => agentApplicationIds.has(id)));
    const availableDrivers = new Set(selectedWorker.drivers
      .map((driver) => driver.name)
      .filter((name) => agentDriverNames.has(name)));
    if (capabilitySelectionOwnerRef.current !== owner) {
      capabilitySelectionOwnerRef.current = owner;
      setApplicationIds([...availableApplications].sort());
      setDriverNames([...availableDrivers].sort());
      return;
    }
    setApplicationIds((current) => current.filter((id) => availableApplications.has(id)));
    setDriverNames((current) => current.filter((name) => availableDrivers.has(name)));
  }, [activeRunId, executionContextsLoaded, selectedClientAgent, selectedExecutionContext, selectedWorker]);

  useEffect(() => {
    if (
      activeRunId
      || !executionContextsLoaded
      || !providerInterfacesLoaded
      || pendingNewTaskTargetRef.current
    ) return;
    if (clientAgentSelectionReady) return;
    const admissibleAgents = agentPresets.filter((agent) => !clientAgentSelectionFailure(agent));
    const matchingAgent = exactClientAgentConfiguration(
      admissibleAgents,
      eeId,
      workerInterfaceId,
      providerInterfaceId,
      providerModel,
      reasoningEffort,
      serviceTier,
    );
    if (matchingAgent) {
      setAgentPresetId(matchingAgent.id);
      setNewTaskTargetResolutionBlocked(false);
      setError("");
      return;
    }
    if (agentPresetId) setAgentPresetId("");
    if (workerInterfaceId) setWorkerInterfaceId("");
    if (providerInterfaceId) setProviderInterfaceId("");
    if (providerModel) setProviderModel("");
    if (reasoningEffort) setReasoningEffort("");
    if (applicationIds.length) setApplicationIds([]);
    if (driverNames.length) setDriverNames([]);
    setNewTaskTargetResolutionBlocked(true);
    setError(selectedClientAgentFailure
      ? `${selectedClientAgent?.name || "Agent"} is no longer admissible: ${selectedClientAgentFailure}. Choose an Agent before sending.`
      : "");
  }, [
    activeRunId,
    agentPresetId,
    agentPresets,
    applicationIds,
    driverNames,
    eeId,
    executionContextsLoaded,
    providerInterfaceId,
    providerModel,
    providerInterfacesLoaded,
    reasoningEffort,
    serviceTier,
    clientAgentSelectionReady,
    selectedClientAgent,
    selectedClientAgentFailure,
    workerInterfaceId,
  ]);

  useEffect(() => {
    const target = pendingNewTaskTargetRef.current;
    if (!executionContextsLoaded || !providerInterfacesLoaded || !target || activeRunIdRef.current) return;
    pendingNewTaskTargetRef.current = null;
    if (target.agentPresetId) {
      chooseAgentPreset(target.agentPresetId);
      return;
    }
    const requestedEeId = target.eeId;
    const requestedContext = executionContexts.find((context) => context.eeId === requestedEeId) ?? null;
    const defaults = projectAgents(agentPresets, requestedEeId);
    const targetResolutionError = !requestedContext
      ? `The selected project '${requestedEeId}' is no longer available. Choose a project before sending.`
      : "";
    setEeId(requestedEeId);
    setNewTaskTargetResolutionBlocked(!requestedContext || defaults.length !== 1);
    setError(targetResolutionError);
    if (defaults.length === 1) chooseAgentPreset(defaults[0].id);
    else {
      setAgentPresetId("");
      setWorkerInterfaceId("");
      setApplicationIds([]);
      setDriverNames([]);
      setAccessMode("project_access");
    }
  }, [agentPresets, executionContexts, executionContextsLoaded, providerInterfacesLoaded]);

  useEffect(() => {
    if (!enabled || !runtimeReady || storageBlockReason || !queueReady || !runsLoaded) return;
    const ready = queueDispatchCandidates(queueStore.snapshot, runs);
    for (const item of ready) {
      if (interfaceFailureReason(item.runKey, item.providerInterfaceId)) continue;
      if (submitLockRef.current.has(item.runKey) || steeringComposerRef.current.has(item.runKey)) continue;
      const projected = runs.find((run) => run.runId === item.runKey)!;
      const signature = JSON.stringify([projected.lastMovementSeq, queuedTurns.filter((peer) => peer.runKey === item.runKey).map((peer) => [peer.id, peer.messageId, peer.status])]);
      if (queueAttemptedProjection.get(item.runKey) === signature) continue;
      queueAttemptedProjection.set(item.runKey, signature);
      void executeTurnSequence(queuedSubmission(item)).catch((failure) => {
        if (deliveryScopeCurrent()) setError(formatRuntimeError(failure));
      });
    }
  }, [runs, runsLoaded, enabled, runtimeReady, storageBlockReason, interfaceFailures, queueReady, queuedTurns, pausedQueueRunKeys, inFlightRunKeys, steeringComposerPendingRunKeys]);

  const renderedMessages = useMemo<ChatRenderMessage[]>(() => {
    const projected = messages.map(toRender);
    const hasProjectedReply = projected.some((message) => message.role === "assistant");
    const visiblePending = pendingByRunKey[activeRunKey] ?? [];
    const showPending = activeRunInFlight || !hasProjectedReply || visiblePending.some((message) => message.error);
    const rememberedSteering = pendingSteeringMessages
      .filter((entry) => entry.runKey === activeRunKey)
      .map((entry) => entry.message);
    const visibleSteering = [...new Map([...rememberedSteering, ...pendingQueueSteering(queuedTurns, activeRunKey)]
      .map((message) => [message.id, message])).values()];
    return withRunOperationTargets(mergeRenderedMessages(projected, visiblePending, visibleSteering, showPending), runs);
  }, [activeRunInFlight, activeRunKey, messages, pendingByRunKey, pendingSteeringMessages, queuedTurns, runs]);

  const activeMessageCount = renderedMessages.length;
  const submitting = renderedMessages.some((message) => (
    (!message.runId || message.runId === activeRunId)
    && message.eventData?.work_status === "submitting"
  ));

  const pinnedMessages = useMemo(() => {
    const excerptIds = new Set(activeRun?.pinnedContextItems.map((item) => item.artifactId) ?? []);
    const byArtifact = new Map<string, ChatRenderMessage>();
    for (const message of renderedMessages) {
      if (!message.artifactId) continue;
      const current = byArtifact.get(message.artifactId);
      if (!current || (current.eventData?._movement_evidence === true && message.eventData?._movement_evidence !== true)) {
        byArtifact.set(message.artifactId, message);
      }
    }
    return (activeRun?.pinnedArtifactIds ?? []).filter((artifactId) => !excerptIds.has(artifactId)).map((artifactId) => byArtifact.get(artifactId) ?? {
      id: `pinned:${artifactId}`,
      runId: activeRun?.runId,
      role: "system" as const,
      text: artifactId,
      createdAtMs: 0,
      artifactId,
      terminal: true,
    });
  }, [activeRun, renderedMessages]);

  const pinnedContextItems = activeRun?.pinnedContextItems ?? [];

  const inputHistory = useMemo(() => {
    const seen = new Set<string>();
    return [...messages]
      .reverse()
      .flatMap((message) => {
        const value = message.role === "user" && (message.eventKind === "message" || message.eventKind === "steering")
          ? message.text.trim()
          : "";
        if (!value || seen.has(value)) return [];
        seen.add(value);
        return [value];
      })
      .slice(0, 100);
  }, [messages]);

  const visibleQueuedTurns = useMemo<ChatQueuedTurnView[]>(() => {
    const runKey = chatDraftKey(activeRunId);
    return visibleQueueTurns(queuedTurns, runKey).map(({ id, text, attachments, createdAtMs, status, error }) => ({
      id, text, attachments, createdAtMs, status, error,
    }));
  }, [activeRunId, queuedTurns]);

  useEffect(() => {
    if (!queueReady || loadingMessages) return;
    const failed = queuedTurns.find((item) => item.runKey === chatDraftKey(activeRunId)
      && item.intent === "turn" && !Number.isFinite(item.queuedAfterSeq) && ["unknown", "failed"].includes(item.status));
    if (!failed || text.trim() || attachments.length
      || messages.some((message) => message.id === failed.messageId && message.role === "user")) return;
    if (!text.trim() && !attachments.length) {
      const restored = { ...failed, messageId: failed.status === "unknown" ? failed.messageId : "",
        messageHadAttachments: failed.hadAttachments, agentPresetId: failed.agentPresetId ?? "",
        applicationIds: failed.applicationIds ?? [], driverNames: failed.driverNames ?? [],
        accessMode: failed.accessMode ?? "project_access" as TaskAccessMode, continuation: failed.continuation ?? null };
      writeDraft(activeRunId, restored);
      applyDraft(restored);
    }
    setError(failed.error || "Couldn't send your message. Try again.");
    void replaceQueuedTurns((items) => items.filter((item) => item.id !== failed.id));
  }, [queueReady, queuedTurns, activeRunId, loadingMessages, text, attachments]);

  useEffect(() => {
    if (!draftMessageId || !messages.some((message) => message.id === draftMessageId && message.role === "user")) return;
    consumeDraft(activeRunId, text, attachments);
    completeSubmissionIntent(activeRunId, draftMessageId);
    setError("");
  }, [messages, draftMessageId, activeRunId]);

  const queueRecoveryReads = useMemo(() => new Map<string, string>(), [accessProfile, runtimeHome]);
  const [queueRecoveryPass, setQueueRecoveryPass] = useState(0);
  useEffect(() => {
    if (!enabled || !runtimeReady || !queueReady || !runsLoaded) return;
    let cancelled = false;
    const candidates = runs.flatMap((run) => {
      const pending = queuedTurns.filter((item) => item.runKey === run.runId && ["accepted", "unknown", "sending"].includes(item.status));
      if (!pending.length) return [];
      const signature = JSON.stringify([run.lastMovementSeq, pending.map((item) => [item.id, item.status, item.admittedSeq])]);
      return queueRecoveryReads.get(run.runId) === signature ? [] : [{ run, pending, signature }];
    });
    const batch = candidates.slice(0, 4);
    if (!batch.length) return;
    void Promise.all(batch.map(async ({ run, pending, signature }) => {
      queueRecoveryReads.set(run.runId, signature);
      const history = await refreshMessages(run.runId);
      if (!history || !queueOwner.active || !deliveryScopeCurrent()) return;
      const updates = pending.flatMap((item) => {
        const admission = history.find((message) => message.id === item.messageId && message.role === "user" && message.runId === run.runId);
        const sequence = admission?.movementSeq ?? item.admittedSeq;
        const completed = sequence !== undefined && history.some((message) => message.runId === run.runId && message.direction === "out" && message.role === "assistant" && message.terminal
          && ["message", "interrupted_result", "error"].includes(message.eventKind) && message.movementSeq > sequence);
        if (item.intent === "steer") return admission ? [{ id: item.id, remove: true, sequence }] : [];
        if (completed) return [{ id: item.id, remove: true, sequence }];
        return admission && item.status !== "accepted" ? [{ id: item.id, remove: false, sequence }] : [];
      });
      if (!updates.length) return;
      const interrupted = updates.some((update) => update.remove) && operatorTurnResumeAvailable(history, false);
      await queueStore.change((state) => ({
        pausedRunKeys: interrupted ? [...new Set([...state.pausedRunKeys, run.runId])] : state.pausedRunKeys,
        turns: state.turns.flatMap((item) => {
          const update = item.runKey === run.runId ? updates.find((entry) => entry.id === item.id) : undefined;
          return !update ? [item] : update.remove ? [] : [{ ...item, status: "accepted", admittedSeq: update.sequence, error: undefined }];
        }),
      }));
    })).catch((failure) => { if (!cancelled) setError(formatRuntimeError(failure)); }).finally(() => {
      if (queueOwner.active && deliveryScopeCurrent() && candidates.length > batch.length) setQueueRecoveryPass((pass) => pass + 1);
    });
    return () => { cancelled = true; };
  }, [enabled, runtimeReady, queueReady, runsLoaded, runs, queuedTurns, queueRecoveryPass]);

  const artifactWriterAvailable = canSubmitOperatorTurn();
  const steeringWriterAvailable = canSubmitOperatorSteer();
  const activeProviderModel = activeProvider
    ? providerModels(activeProvider).find((model) => model.model === providerModel) ?? null
    : null;
  const providerDeclared = providerSelectionReady && Boolean(activeProvider && activeProviderModel);
  const reasoningOptions = activeProvider && activeProviderModel
    ? providerReasoningEfforts(activeProvider, activeProviderModel.model) : [];
  const reasoningSelectionValid = reasoningOptions.length
    ? reasoningOptions.includes(reasoningEffort) : reasoningEffort === "";
  const serviceTierSelectionValid = Boolean(activeProvider && providerServiceTierAvailable(activeProvider, providerModel, serviceTier ?? ""));
  const canQueueOnActiveRun = replyOpen && replyRunKey === chatDraftKey(activeRunId);
  const canSteer = Boolean(
    !runtimeBlockReason && steeringWriterAvailable
    && activeRun?.writable
    && replyOpen
    && canQueueOnActiveRun
    && activeRun?.turnOpen
    && activeRun.chatInterfaceId,
  );
  const steerableMessageId = latestSteerableUserMessageId(renderedMessages, activeRunId, canSteer);
  const steerableMessageIdRef = useRef(steerableMessageId);
  steerableMessageIdRef.current = steerableMessageId;
  const activeRunSubmitAllowed = activeRun?.writable && canQueueOnActiveRun;
  const canSendQueued = Boolean(enabled && runtimeReady && !runtimeBlockReason && queueReady && activeRun?.writable && !loadingMessages
    && !activeRunSteeringPending && !applicationMutationPending
    && (replyOpen ? canSteer : artifactWriterAvailable && !activeRun.turnOpen)
    && !queuedTurns.some((item) => item.runKey === activeRunKey
      && (item.status === "sending" || item.status === "unknown" || (!replyOpen && item.status === "accepted"))));
  const hasSubmissionPayload = Boolean(text.trim() || attachments.length);
  const submitBlockReason = runtimeBlockReason || (!enabled
    ? "This surface is read-only."
    : activeRun?.archived
      ? "This task is archived. Restore it to send messages."
    : !queueReady
      ? queueError || "Loading saved messages for this runtime Home…"
    : !runtimeReady
      ? "The runtime is unavailable. Your message has not been sent."
      : loadingMessages
        ? "Task history is still loading."
        : !artifactWriterAvailable
          ? "No local writer is available for operator turns."
          : !replyOpen && (activeRunSteeringPending || queuedTurns.some((item) => item.runKey === activeRunKey && ["sending", "unknown"].includes(item.status)))
            ? "A message is awaiting delivery confirmation. Check its status below."
            : applicationMutationPending
              ? "Wait for the application change to finish."
              : replyOpen && !activeRunSubmitAllowed
                ? "This task cannot queue a follow-up right now."
                : !activeRunId && executionContextsLoaded && providerInterfacesLoaded && !clientAgentSelectionReady
                  ? "Choose an Agent before sending."
                : providerInterfacesRefreshing
                  ? "Refreshing available models…"
                : providerInterfaceError || !providerDeclared
                  ? !providerInterfacesLoaded
                  ? "Loading model interfaces…"
                    : providerInterfaceError || providerInterfaces.find((provider) => provider.id === resolvedProviderInterfaceId)?.modelCatalogError || `${resolvedProviderInterfaceId || "Provider interface"} does not admit model '${providerModel || "not selected"}' for this task.`
                  : !serviceTierSelectionValid
                    ? `Model '${providerModel}' does not advertise service tier '${activeProvider ? providerSelectedServiceTier(activeProvider, providerModel, serviceTier) : serviceTier}'. Choose an available speed.`
                  : !reasoningSelectionValid
                    ? "The selected reasoning level is no longer supported by this model. Choose a supported level."
                  : !executionContextReady
                    ? executionContextError || (!executionContextsLoaded ? "Loading task capabilities…" : "")
                      || selectedClientAgentCapabilityFailure || newTaskAuthoringError || "Choose an Agent available in a projected execution environment."
                    : workspaceSelectionError
                      ? workspaceSelectionError
                      : !hasSubmissionPayload
                        ? "Type a message or attach a file."
                        : "");
  const editingQueuedTurn = queuedTurns.find((item) => item.runKey === activeRunKey && item.status === "editing" && item.messageId === draftMessageId && item.editOwner === queueOwner.id);
  const canSubmit = !runtimeBlockReason && !activeRun?.archived && (Boolean(editingQueuedTurn && queueReady && hasSubmissionPayload) || submitBlockReason === "");
  const canQueue = enabled && runtimeReady && !runtimeBlockReason && queueReady && activeRun?.writable === true && canQueueOnActiveRun && hasSubmissionPayload && providerDeclared && !providerInterfacesRefreshing && !providerInterfaceError && reasoningSelectionValid && serviceTierSelectionValid;

  function selectRun(runId: string) {
    if (runId === activeRunIdRef.current) return;
    pendingNewTaskTargetRef.current = null;
    setNewTaskTargetResolutionBlocked(false);
    saveCurrentDraft(activeRunId);
    const nextDraft = readDraft(runId);
    const nextRun = runs.find((run) => run.runId === runId);
    const storedAgentPresetId = nextDraft?.agentPresetId ?? "";
    const inferredAgent = nextRun && executionContextsLoaded && providerInterfacesLoaded
      ? clientAgentForRun(allAgentPresets, nextRun)
      : null;
    const resolvedAgentPresetId = nextRun && executionContextsLoaded && providerInterfacesLoaded
      ? inferredAgent?.id ?? ""
      : storedAgentPresetId;
    const cachedMessages = messageCacheRef.current.get(runId);
    messageReadGenerationRef.current.set(runId, (messageReadGenerationRef.current.get(runId) ?? 0) + 1);
    activeRunIdRef.current = runId;
    setActiveRunId(runId);
    setMessages(cachedMessages ?? []);
    setLoadingMessages(Boolean(runId));
    applyDraft({
      messageId: nextDraft?.messageId ?? "",
      messageHadAttachments: nextDraft?.messageHadAttachments ?? false,
      text: nextDraft?.text ?? "",
      attachments: nextDraft?.attachments ?? [],
      agentPresetId: resolvedAgentPresetId,
      eeId: nextDraft?.eeId || nextRun?.eeId || "",
      workerInterfaceId: nextDraft?.workerInterfaceId || nextRun?.workerChoiceInterfaceId || nextRun?.workerInterfaceId || "",
      providerInterfaceId: nextRun?.providerInterfaceId || nextDraft?.providerInterfaceId || "",
      providerModel: nextDraft?.providerModel || nextRun?.providerModel || "",
      reasoningEffort: nextDraft?.reasoningEffort || nextRun?.reasoningEffort || "",
      serviceTier: nextDraft?.serviceTier ?? nextRun?.serviceTier ?? "",
      applicationIds: nextRun?.connectedApplicationIds ?? [],
      driverNames: nextDraft?.driverNames ?? [],
      accessMode: nextRun
        ? taskAccessModeFromDoorHostSlots(nextRun.doorHostSlots)
        : nextDraft?.accessMode ?? "project_access",
      continuation: nextDraft?.continuation ?? null,
    });
    setError("");
    setMessageQuery("");
    resetInputHistory();
  }

  function createChat(targetEeId = "", targetAgentPresetId = "", initialText = "") {
    saveCurrentDraft(activeRunId);
    deleteDraft("");
    activeRunIdRef.current = "";
    setActiveRunId("");
    setMessages([]);
    setLoadingMessages(false);
    const defaults = runsLoaded
      ? resolveNewTaskDefaults(runs, loadChatSettings(accessProfile))
      : null;
    const requestedAgent = targetAgentPresetId
      ? agentPresets.find((candidate) => candidate.id === targetAgentPresetId) ?? null
      : null;
    const explicitEeId = requestedAgent?.projectEeId || targetEeId.trim();
    const requestedEeId = explicitEeId || defaults?.eeId || "";
    const requestedContext = requestedEeId && executionContextsLoaded
      ? executionContexts.find((context) => context.eeId === requestedEeId) ?? null
      : null;
    const defaultAgents = projectAgents(agentPresets, requestedEeId);
    const selectedAgent = targetAgentPresetId
      ? requestedAgent
      : defaultAgents.length === 1 ? defaultAgents[0] : null;
    const catalogReady = executionContextsLoaded && providerInterfacesLoaded;
    const selectedAgentFailure = selectedAgent && catalogReady
      ? clientAgentSelectionFailure(selectedAgent)
      : "";
    const usableAgent = selectedAgent && catalogReady && !selectedAgentFailure ? selectedAgent : null;
    pendingNewTaskTargetRef.current = !catalogReady && (requestedEeId || targetAgentPresetId)
      ? { eeId: requestedEeId, agentPresetId: targetAgentPresetId || selectedAgent?.id || "" }
      : null;
    const targetResolutionError = targetAgentPresetId && catalogReady && !requestedAgent
      ? `Agent '${targetAgentPresetId}' is no longer available.`
      : requestedEeId && catalogReady && !requestedContext
      ? `The selected project '${requestedEeId}' is no longer available. Choose a project before sending.`
      : selectedAgentFailure
        ? `${selectedAgent?.name || "Agent"} cannot start a task: ${selectedAgentFailure}`
        : "";
    const nextEeId = selectedAgent?.projectEeId ?? requestedEeId;
    const nextWorkerInterfaceId = usableAgent?.workerInterfaceId ?? "";
    const capabilityDefaults = defaultCapabilitySelection(
      executionContexts,
      nextEeId,
      nextWorkerInterfaceId,
    );
    const nextProviderInterfaceId = usableAgent?.providerInterfaceId ?? defaults?.providerInterfaceId ?? "";
    const nextProviderModel = usableAgent?.providerModel
      ?? (defaults?.providerInterfaceId === nextProviderInterfaceId ? defaults.providerModel : "");
    applyDraft({
      messageId: "",
      messageHadAttachments: false,
      text: initialText,
      attachments: [],
      agentPresetId: selectedAgent?.id ?? targetAgentPresetId,
      eeId: nextEeId,
      workerInterfaceId: nextWorkerInterfaceId,
      providerInterfaceId: nextProviderInterfaceId,
      providerModel: nextProviderModel,
      reasoningEffort: usableAgent?.reasoningEffort ?? defaults?.reasoningEffort ?? "",
      serviceTier: usableAgent?.serviceTier ?? defaults?.serviceTier ?? "",
      applicationIds: usableAgent?.applicationIds ?? capabilityDefaults.applicationIds,
      driverNames: usableAgent?.driverNames ?? capabilityDefaults.driverNames,
      accessMode: requestedContext?.defaultTaskAccessMode ?? "project_access",
      continuation: null,
    });
    setNewTaskTargetResolutionBlocked(Boolean(
      targetResolutionError
      || requestedEeId && (!catalogReady || !requestedContext || !selectedAgent),
    ));
    setError(targetResolutionError);
    setMessageQuery("");
    resetInputHistory();
    if (usableAgent) chooseAgentPreset(usableAgent.id);
  }

  function continueInNewTask(message: ChatRenderMessage): boolean {
    const declaredContinuation = message.eventData?.continuation;
    let sourceRunId = message.runId || activeRunIdRef.current;
    let sourceArtifactId = message.artifactId || "";
    if (declaredContinuation !== undefined) {
      if (!declaredContinuation || typeof declaredContinuation !== "object" || Array.isArray(declaredContinuation)) {
        setError("The terminal artifact declares malformed continuation coordinates.");
        return false;
      }
      const declared = declaredContinuation as Record<string, unknown>;
      sourceRunId = eventString(declared.source_run_id);
      sourceArtifactId = eventString(declared.source_artifact_id);
      if (!sourceRunId || !sourceArtifactId) {
        setError("The terminal artifact declares incomplete continuation coordinates.");
        return false;
      }
    }
    if (!sourceRunId || !sourceArtifactId || message.role !== "assistant" || message.terminal === false) {
      setError("Only a completed assistant message with artifact provenance can start a new task.");
      return false;
    }
    saveCurrentDraft(activeRunId);
    const nextContinuation = { sourceRunId, sourceArtifactId };
    const sourceRun = runs.find((run) => run.runId === sourceRunId);
    if (!sourceRun) {
      setError(`Continuation source run '${sourceRunId}' is not present in the chat projection.`);
      return false;
    }
    const nextEeId = sourceRun?.eeId ?? eeId;
    const sourceAgent = clientAgentForRun(allAgentPresets, sourceRun);
    const preserveFailedDraft = message.error === true && declaredContinuation !== undefined;
    const nextText = preserveFailedDraft ? text : "";
    const nextAttachments = preserveFailedDraft ? attachments : [];
    const nextDraft = {
      messageId: "",
      messageHadAttachments: false,
      text: nextText,
      attachments: nextAttachments,
      agentPresetId: sourceAgent?.id ?? "",
      eeId: nextEeId,
      workerInterfaceId: sourceAgent?.workerInterfaceId ?? "",
      providerInterfaceId: sourceAgent ? sourceRun.providerInterfaceId : "",
      providerModel: sourceAgent ? sourceRun.providerModel : "",
      reasoningEffort: sourceAgent ? sourceRun.reasoningEffort : "", serviceTier: sourceAgent ? sourceRun.serviceTier : "",
      applicationIds: sourceAgent ? [...sourceAgent.applicationIds] : [],
      driverNames: sourceAgent ? [...sourceAgent.driverNames] : [],
      accessMode: taskAccessModeFromDoorHostSlots(sourceRun.doorHostSlots),
      continuation: nextContinuation,
    };
    writeDraft("", nextDraft);
    activeRunIdRef.current = "";
    pendingNewTaskTargetRef.current = null;
    setNewTaskTargetResolutionBlocked(!sourceAgent);
    setActiveRunId("");
    setMessages([]);
    setLoadingMessages(false);
    applyDraft(nextDraft);
    setError(sourceAgent
      ? ""
      : "The source task does not identify one current Agent. Choose an Agent before sending the continuation.");
    setMessageQuery("");
    return true;
  }

  function startReviewTask(seed: ReviewTaskSeed) {
    if (submitLockRef.current.has(chatDraftKey(""))) {
      setError("A new task is still being authored. Wait for its run identity before opening another new task.");
      return;
    }
    const sourceRun = runs.find((run) => run.runId === seed.sourceRunId);
    if (!sourceRun) {
      setError(`Run '${seed.sourceRunId}' is not present in the chat projection.`);
      return;
    }
    const sourceAgent = clientAgentForRun(allAgentPresets, sourceRun);
    if (!sourceAgent) {
      setError("The source task does not resolve to one exact Agent worker template. Choose an Agent before starting a review task.");
      return;
    }
    const context = executionContexts.find((candidate) => candidate.eeId === sourceRun.eeId);
    const reviewWorker = context?.workers.find((worker) => worker.id === sourceAgent.workerInterfaceId);
    if (!reviewWorker) {
      setError(`${sourceAgent.name} is no longer projected in '${sourceRun.eeId}'.`);
      return;
    }
    saveCurrentDraft(activeRunId);
    const sourceArtifact = [...messages].reverse().find((message) => (
      message.runId === seed.sourceRunId
      && message.role === "assistant"
      && message.terminal
      && Boolean(message.artifactId)
    ));
    const nextContinuation = sourceArtifact?.artifactId
      ? { sourceRunId: seed.sourceRunId, sourceArtifactId: sourceArtifact.artifactId }
      : null;
    const nextAttachment: ChatAttachment = {
      id: `${seed.attachment.name}:${seed.attachment.size}:${seed.attachment.lastModified}`,
      file: seed.attachment,
      name: seed.attachment.name,
      size: seed.attachment.size,
      type: seed.attachment.type || "application/json",
      label: attachmentLabel(seed.attachment),
      lastModified: seed.attachment.lastModified,
    };
    const nextProviderInterfaceId = sourceRun.providerInterfaceId || providerInterfaceId;
    const nextReasoningEffort = sourceRun.reasoningEffort || reasoningEffort;
    if (!reviewWorker.providerInterfaceIds.includes(nextProviderInterfaceId)) {
      setError(`${sourceAgent.name} cannot use model interface '${nextProviderInterfaceId}' in '${sourceRun.eeId}'.`);
      return;
    }
    const nextDraft = {
      messageId: "",
      messageHadAttachments: false,
      text: seed.prompt,
      attachments: [nextAttachment],
      agentPresetId: sourceAgent.id,
      eeId: sourceRun.eeId,
      workerInterfaceId: sourceAgent.workerInterfaceId,
      providerInterfaceId: nextProviderInterfaceId,
      providerModel: sourceRun.providerModel || providerModel,
      reasoningEffort: nextReasoningEffort,
      serviceTier,
      applicationIds: [...sourceAgent.applicationIds],
      driverNames: [...sourceAgent.driverNames],
      accessMode: taskAccessModeFromDoorHostSlots(sourceRun.doorHostSlots),
      continuation: nextContinuation,
    };
    writeDraft("", nextDraft);
    activeRunIdRef.current = "";
    pendingNewTaskTargetRef.current = null;
    setNewTaskTargetResolutionBlocked(false);
    setActiveRunId("");
    setMessages([]);
    setLoadingMessages(false);
    applyDraft(nextDraft);
    setError("");
    setMessageQuery("");
    resetInputHistory();
  }

  function clearContinuation() {
    setContinuation(null);
    setError("");
  }

  async function steerUserMessage(message: ChatRenderMessage, editedText: string): Promise<boolean> {
    const nextText = editedText.trim();
    if (!nextText || !canSteer || message.id !== steerableMessageIdRef.current
      || message.runId !== activeRunIdRef.current || message.role !== "user" || message.eventKind !== "message"
      || message.pending || message.error) {
      setError("This reply is no longer accepting edits. Your text has been kept.");
      return false;
    }
    return submitSteeringMessage({ attachments: [], fromComposer: false,
      editedMessageId: message.id, runKey: chatDraftKey(message.runId), text: nextText });
  }

  async function editUserMessage(message: ChatRenderMessage, editedText?: string): Promise<boolean> {
    const nextText = (editedText ?? message.text).trim();
    const run = runs.find((candidate) => candidate.runId === message.runId);
    const latest = renderedMessages.filter((candidate) => candidate.role === "user").at(-1);
    if (!nextText || !run?.writable || run.archived || run.turnOpen || replyOpen
      || message.runId !== activeRunIdRef.current || latest?.id !== message.id
      || message.eventKind !== "message" || !message.artifactId || !message.contentHash
      || !runtimeReady || !queueReady || queuedTurnsRef.current.some((item) => item.runKey === chatDraftKey(run.runId))) {
      setError("Only the most recent message in an idle task can be edited. Your text has been kept.");
      return false;
    }
    const replacesMessage = { id: message.artifactId, content: message.contentHash };
    const messageId = `msg_${randomId()}`;
    return new Promise<boolean>((resolve) => {
      void beginSubmission({
      messageId, recoverPreparedMessage: false, runId: run.runId, runKey: chatDraftKey(run.runId),
      replacesMessage,
      text: nextText, attachments: [], eeId: run.eeId, workerInterfaceId: run.workerChoiceInterfaceId || run.workerInterfaceId,
      providerInterfaceId: run.providerInterfaceId, providerModel: run.providerModel,
      reasoningEffort: run.reasoningEffort, serviceTier: run.serviceTier,
      applicationIds: [], driverNames: [], accessMode: "project_access", endpoint,
      continuation: null, clientSurfaceRole: run.clientSurfaceRole, agentPresetId: "",
      }, undefined, () => resolve(true)).then(() => resolve(false), (error) => {
        setError(formatProjectionError(error));
        resolve(false);
      });
    });
  }

  async function togglePinnedMessage(message: ChatRenderMessage) {
    const runId = message.runId || activeRunIdRef.current;
    const artifactId = message.artifactId || "";
    if (!runId || !artifactId || pinningArtifactIds.includes(artifactId)) return;
    const currentRun = runs.find((run) => run.runId === runId);
    if (currentRun?.archived) { setError("Restore this archived task before changing its context."); return; }
    if (!currentRun) {
      setError(`Run '${runId}' is not present in the chat projection.`);
      return;
    }
    const pinned = !currentRun.pinnedArtifactIds.includes(artifactId);
    const previous = currentRun.pinnedArtifactIds;
    setPinningArtifactIds((current) => [...current, artifactId]);
    setRuns((current) => current.map((run) => run.runId === runId ? {
      ...run,
      pinnedArtifactIds: pinned
        ? Array.from(new Set([...run.pinnedArtifactIds, artifactId]))
        : run.pinnedArtifactIds.filter((id) => id !== artifactId),
    } : run));
    try {
      const outcome = await updateOperatorChatPins({ runId, artifactId, pinned });
      setRuns((current) => current.map((run) => run.runId === runId
        ? { ...run, pinnedArtifactIds: outcome.pinnedArtifactIds, pinnedContextItems: outcome.pinnedContextItems }
        : run));
      setError("");
    } catch (failure) {
      setRuns((current) => current.map((run) => run.runId === runId
        ? { ...run, pinnedArtifactIds: previous }
        : run));
      setError(formatRuntimeError(failure));
    } finally {
      setPinningArtifactIds((current) => current.filter((id) => id !== artifactId));
    }
  }

  async function pinMessageExcerpt(
    message: ChatRenderMessage,
    selection: { text: string; start: number; end: number },
  ) {
    const runId = message.runId || activeRunIdRef.current;
    const sourceArtifactId = message.artifactId || "";
    if (projectedRunsRef.current.find((run) => run.runId === runId)?.archived) { setError("Restore this archived task before changing its context."); return; }
    if (!runId || !sourceArtifactId || !selection.text.trim()) return;
    const pendingId = `excerpt:${sourceArtifactId}:${selection.start}:${selection.end}`;
    if (pinningArtifactIds.includes(pendingId)) return;
    setPinningArtifactIds((current) => [...current, pendingId]);
    try {
      const outcome = await updateOperatorChatPins({
        runId,
        artifactId: sourceArtifactId,
        pinned: true,
        excerpt: {
          sourceMessageId: message.id,
          text: selection.text,
          start: selection.start,
          end: selection.end,
          role: message.role,
        },
      });
      setRuns((current) => current.map((run) => run.runId === runId
        ? { ...run, pinnedArtifactIds: outcome.pinnedArtifactIds, pinnedContextItems: outcome.pinnedContextItems }
        : run));
      setError("");
    } catch (failure) {
      setError(formatRuntimeError(failure));
    } finally {
      setPinningArtifactIds((current) => current.filter((id) => id !== pendingId));
    }
  }

  async function unpinContextItem(item: ChatPinnedContextItem) {
    const runId = activeRunIdRef.current;
    if (projectedRunsRef.current.find((run) => run.runId === runId)?.archived) { setError("Restore this archived task before changing its context."); return; }
    if (!runId || pinningArtifactIds.includes(item.artifactId)) return;
    setPinningArtifactIds((current) => [...current, item.artifactId]);
    try {
      const outcome = await updateOperatorChatPins({ runId, artifactId: item.artifactId, pinned: false });
      setRuns((current) => current.map((run) => run.runId === runId
        ? { ...run, pinnedArtifactIds: outcome.pinnedArtifactIds, pinnedContextItems: outcome.pinnedContextItems }
        : run));
      setError("");
    } catch (failure) {
      setError(formatRuntimeError(failure));
    } finally {
      setPinningArtifactIds((current) => current.filter((id) => id !== item.artifactId));
    }
  }

  function chooseProviderInterface(nextProviderInterfaceId: string) {
    if (activeRunIdRef.current) return;
    const provider = selectableProviderInterfaces.find((candidate) => candidate.id === nextProviderInterfaceId);
    const model = provider ? providerDefaultModel(provider) : null;
    const nextProviderModel = model?.model ?? "";
    const nextReasoningEffort = provider && nextProviderModel
      ? providerDefaultReasoningEffort(provider, nextProviderModel)
      : "";
    setProviderInterfaceId(nextProviderInterfaceId);
    setProviderModel(nextProviderModel);
    setReasoningEffort(nextReasoningEffort);
    saveChatSettings({
      providerInterfaceId: nextProviderInterfaceId,
      providerModel: nextProviderModel,
      reasoningEffort: nextReasoningEffort,
      serviceTier,
    }, accessProfile);
  }

  function chooseProviderModel(nextProviderInterfaceId: string, nextProviderModel: string, requestedEffort?: string) {
    if (activeRunIdRef.current && nextProviderInterfaceId !== resolvedProviderInterfaceId) return;
    const provider = activeRunIdRef.current
      ? declaredActiveProvider
      : selectableProviderInterfaces.find((candidate) => candidate.id === nextProviderInterfaceId);
    if (!provider) return;
    const model = providerModels(provider).find((candidate) => candidate.model === nextProviderModel);
    if (!model) return;
    const efforts = providerReasoningEfforts(provider, model.model);
    if (requestedEffort !== undefined && !efforts.includes(requestedEffort)
      && !(requestedEffort === "" && efforts.length === 0)) return;
    const nextReasoningEffort = requestedEffort ?? providerDefaultReasoningEffort(provider, model.model);
    setProviderInterfaceId(provider.id);
    setProviderModel(model.model);
    setReasoningEffort(nextReasoningEffort);
    if (!activeRunIdRef.current) {
      saveChatSettings({
        providerInterfaceId: provider.id,
        providerModel: model.model,
        reasoningEffort: nextReasoningEffort,
        serviceTier,
      }, accessProfile);
    }
  }

  function chooseExecutionEnvironment(nextEeId: string) {
    if (activeRunIdRef.current) return;
    pendingNewTaskTargetRef.current = null;
    setNewTaskTargetResolutionBlocked(false);
    setError("");
    const nextContext = executionContexts.find((context) => context.eeId === nextEeId) ?? null;
    if (!nextContext) {
      setAgentPresetId("");
      setEeId("");
      setWorkerInterfaceId("");
      setApplicationIds([]);
      setDriverNames([]);
      setAccessMode("project_access");
      setNewTaskTargetResolutionBlocked(true);
      setError(`The selected project '${nextEeId}' is no longer available. Choose a project before sending.`);
      return;
    }
    const defaults = projectAgents(agentPresets, nextContext.eeId);
    if (defaults.length === 1) {
      chooseAgentPreset(defaults[0].id);
      return;
    }
    setAgentPresetId("");
    setEeId(nextContext.eeId);
    setWorkerInterfaceId("");
    setApplicationIds([]);
    setDriverNames([]);
    setAccessMode(nextContext.defaultTaskAccessMode);
    setNewTaskTargetResolutionBlocked(true);
    setError("");
  }

  function clientAgentSelectionFailure(preset: ClientAgentDefinition): string {
    const context = executionContexts.find((candidate) => candidate.eeId === preset.projectEeId);
    const worker = context?.workers.find((candidate) => candidate.id === preset.workerInterfaceId);
    const declaredProvider = providerInterfaces.find((candidate) => candidate.id === preset.providerInterfaceId);
    const provider = worker && declaredProvider
      ? providerForExecutionContext(declaredProvider, worker)
      : undefined;
    const defaultModel = provider && preset.providerModel
      ? providerModels(provider).find((model) => model.model === preset.providerModel) : null;
    const supportedReasoningEfforts = provider && defaultModel
      ? providerReasoningEfforts(provider, defaultModel.model)
      : [];
    const unavailable = !context
      ? `project '${preset.projectEeId}' is unavailable`
      : !worker
        ? `worker template '${preset.workerInterfaceId}' is unavailable in that project`
        : !provider || !worker.providerInterfaceIds.includes(preset.providerInterfaceId)
          ? `model interface '${preset.providerInterfaceId}' is unavailable to that Agent template`
          : !defaultModel
            ? `model '${preset.providerModel || "not selected"}' is unavailable; edit the Agent to choose an admitted model`
          : !providerServiceTierAvailable(provider, defaultModel.model, preset.serviceTier ?? "")
            ? `service tier '${providerSelectedServiceTier(provider, defaultModel.model, preset.serviceTier)}' is unavailable; edit the Agent to choose an advertised speed`
          : (supportedReasoningEfforts.length ? !supportedReasoningEfforts.includes(preset.reasoningEffort) : Boolean(preset.reasoningEffort))
            ? `reasoning level '${preset.reasoningEffort}' is unavailable to that model interface`
            : "";
    if (unavailable || !worker) return unavailable;
    const requestedAccessMode = preset.projectEeId === eeId
      ? accessMode
      : context?.defaultTaskAccessMode ?? "project_access";
    try {
      executionContextTaskCapabilities(
        executionContexts,
        preset.projectEeId,
        preset.workerInterfaceId,
        preset.applicationIds,
        preset.driverNames,
        requestedAccessMode,
      );
    } catch (failure) {
      return failure instanceof Error ? failure.message : String(failure);
    }
    return "";
  }

  function chooseAgentPreset(nextAgentPresetId: string) {
    if (activeRunIdRef.current) return;
    const preset = agentPresets.find((candidate) => candidate.id === nextAgentPresetId);
    pendingNewTaskTargetRef.current = null;
    if (nextAgentPresetId && (!executionContextsLoaded || !providerInterfacesLoaded)) {
      pendingNewTaskTargetRef.current = { eeId: preset?.projectEeId ?? eeId, agentPresetId: nextAgentPresetId };
      setAgentPresetId(nextAgentPresetId);
      if (preset) setEeId(preset.projectEeId);
      setWorkerInterfaceId("");
      setApplicationIds([]);
      setDriverNames([]);
      setNewTaskTargetResolutionBlocked(true);
      setError("");
      return;
    }
    if (!preset) {
      setAgentPresetId("");
      setWorkerInterfaceId("");
      setApplicationIds([]);
      setDriverNames([]);
      setAccessMode("project_access");
      setNewTaskTargetResolutionBlocked(Boolean(nextAgentPresetId));
      setError(nextAgentPresetId ? `Agent '${nextAgentPresetId}' is no longer available.` : "");
      return;
    }
    const presetContext = executionContexts.find((context) => context.eeId === preset.projectEeId);
    const nextAccessMode = preset.projectEeId === eeId
      ? accessMode
      : presetContext?.defaultTaskAccessMode ?? "project_access";
    const unavailable = clientAgentSelectionFailure(preset);
    if (unavailable) {
      setAgentPresetId(preset.id);
      setEeId(preset.projectEeId);
      setWorkerInterfaceId("");
      setApplicationIds([]);
      setDriverNames([]);
      setAccessMode(nextAccessMode);
      setNewTaskTargetResolutionBlocked(true);
      setError(`${preset.name} cannot start a task: ${unavailable}.`);
      return;
    }
    setNewTaskTargetResolutionBlocked(false);
    capabilitySelectionOwnerRef.current = `${preset.projectEeId}\0${preset.workerInterfaceId}\0${preset.id}`;
    setAgentPresetId(preset.id);
    setEeId(preset.projectEeId);
    setWorkerInterfaceId(preset.workerInterfaceId);
    setProviderInterfaceId(preset.providerInterfaceId);
    setProviderModel(preset.providerModel);
    setReasoningEffort(preset.reasoningEffort);
    setServiceTier(preset.serviceTier ?? "");
    setApplicationIds([...preset.applicationIds]);
    setDriverNames([...preset.driverNames]);
    setAccessMode(nextAccessMode);
    setError("");
  }

  function toggleApplication(applicationId: string) {
    if (!selectedWorker?.applications.some((application) => application.id === applicationId)) return;
    const run = runs.find((candidate) => candidate.runId === activeRunIdRef.current) ?? null;
    if (!run) {
      if (!selectedClientAgent?.applicationIds.includes(applicationId)) return;
      setApplicationIds((current) => current.includes(applicationId)
        ? current.filter((id) => id !== applicationId)
        : [...current, applicationId].sort());
      return;
    }
    if (
      accessProfile !== "live"
      || run.turnOpen
      || replyOpen
      || applicationMutationPending
      || !run.recordDigest
    ) return;
    const connected = !run.connectedApplicationIds.includes(applicationId);
    if (connected && !activeTaskAgent?.applicationIds.includes(applicationId)) {
      setError(`Application '${applicationId}' is outside this Task's immutable Agent template.`);
      return;
    }
    setApplicationMutationPending(applicationId);
    void mutateOctopusOsWorkspace({
      action: "configure_task_application",
      runId: run.runId,
      applicationId,
      connected,
      expectedDigest: run.recordDigest,
    }).then(async () => {
      setApplicationIds((current) => connected
        ? [...new Set([...current, applicationId])].sort()
        : current.filter((id) => id !== applicationId));
      setError("");
      await refreshRuns();
    }).catch((failure) => {
      setError(formatRuntimeError(failure));
    }).finally(() => {
      setApplicationMutationPending("");
    });
  }

  function toggleDriver(driverName: string) {
    if (activeRunIdRef.current || !selectedWorker?.drivers.some((driver) => driver.name === driverName)) return;
    if (!selectedClientAgent?.driverNames.includes(driverName)) return;
    setDriverNames((current) => current.includes(driverName)
      ? current.filter((name) => name !== driverName)
      : [...current, driverName].sort());
  }

  function chooseTaskAccessMode(nextAccessMode: TaskAccessMode) {
    if (activeRunIdRef.current) return;
    if (nextAccessMode === "full_access") {
      if (!capabilitySelectionReady) return;
      try {
        executionContextTaskCapabilities(
          executionContexts,
          eeId,
          workerInterfaceId,
          applicationIds,
          driverNames,
          nextAccessMode,
        );
      } catch (failure) {
        setError(failure instanceof Error ? failure.message : String(failure));
        return;
      }
    }
    setAccessMode(nextAccessMode);
    setError("");
  }

  function chooseReasoningEffort(nextReasoningEffort: string) {
    if (!activeProvider || !providerModel) return;
    const next = nextReasoningEffort.trim();
    if (!providerReasoningEfforts(activeProvider, providerModel).includes(next)) return;
    setReasoningEffort(next);
    if (!activeRunIdRef.current) saveChatSettings({ reasoningEffort: next }, accessProfile);
  }

  function chooseServiceTier(next: string) {
    if (!activeProvider || !providerModel || !providerServiceTierAvailable(activeProvider, providerModel, next)) {
      setError(`Model '${providerModel}' does not advertise service tier '${next}'. Choose an available speed.`);
      return;
    }
    setServiceTier(next);
    if (!activeRunIdRef.current) saveChatSettings({ serviceTier: next }, accessProfile);
    setError("");
  }

  function queuedSubmission(item: QueuedChatTurn): ChatTurnSubmission {
    if (!item.clientSurfaceRole) throw new Error("The saved message has no exact client surface identity.");
    return {
      queueId: item.id,
      replacesMessage: item.replacesMessage,
      messageId: item.messageId,
      recoverPreparedMessage: item.hadAttachments && item.attachments.length === 0,
      runId: item.runKey === chatDraftKey("") ? "" : item.runKey,
      runKey: item.runKey,
      text: item.text,
      attachments: item.attachments,
      eeId: item.eeId,
      workerInterfaceId: item.workerInterfaceId,
      providerInterfaceId: item.providerInterfaceId,
      providerModel: item.providerModel,
      reasoningEffort: item.reasoningEffort, serviceTier: item.serviceTier,
      applicationIds: item.applicationIds ?? [],
      driverNames: item.driverNames ?? [],
      accessMode: item.accessMode ?? "project_access",
      endpoint: item.endpoint,
      continuation: item.continuation ?? null,
      clientSurfaceRole: item.clientSurfaceRole,
      agentPresetId: item.agentPresetId ?? "",
    };
  }

  async function enqueueCurrentTurn(body: string) {
    const runKey = chatDraftKey(activeRunIdRef.current);
    if (!replyOpen || runKey !== replyRunKey || !activeRun) {
      setError("Follow-ups can only be queued on the run that is currently working.");
      return;
    }
    const identity = randomId();
    const item: QueuedChatTurn = {
      id: `queued_${identity}`,
      clientSurfaceRole: activeRun.clientSurfaceRole,
      queuedAfterSeq: activeRun.lastMovementSeq,
      messageId: `msg_${identity}`,
      runKey,
      text: body,
      attachments: [...attachments],
      hadAttachments: attachments.length > 0,
      eeId,
      workerInterfaceId,
      providerInterfaceId,
      providerModel,
      reasoningEffort,
      serviceTier,
      endpoint,
      createdAtMs: Date.now(),
      status: "queued",
    };
    if (clientSurfaceRole === "home") Object.assign(item, await currentHomeProviderSelection(activeRun.runId));
    await replaceQueuedTurns((current) => current.some((candidate) => candidate.messageId === item.messageId) ? current : [...current, item]);
    consumeDraft(activeRunId, body, item.attachments);
    resetInputHistory();
    setError("");
  }

  async function executeTurnSequence(initial: ChatTurnSubmission, onAdmitted?: () => void) {
    const owner = queueOwner.id;
    if (!queueOwner.active || !queueReady || !enabled || !runtimeReady || deliveryContextRef.current.storageBlockReason || interfaceFailureReason(initial.runId, initial.providerInterfaceId)) return;
    if (!acquireSubmission(initial.runKey)) {
      if (initial.manual) await replaceQueuedTurns((items) => items.map((item) => item.id === initial.queueId && item.status === "queued" && item.owner === owner
        ? { ...item, status: "failed", owner: undefined, error: "This task already has an in-flight operator turn. This message was not sent." } : item));
      if (chatDraftKey(activeRunIdRef.current) === initial.runKey) {
        setError("This task already has an in-flight operator turn.");
      }
      return;
    }
    let current: ChatTurnSubmission | null = initial;
    let lockedRunKey = initial.runKey;
    if (chatDraftKey(activeRunIdRef.current) === initial.runKey) setError("");

    try {
      while (current) {
        const queueId = current.queueId;
        if (!queueId) throw new Error("Message has no durable client submission record.");
        const claimed = await queueStore.claim(queueId, owner, false, current.manual === true);
        if (!claimed) {
          if (current.manual) await replaceQueuedTurns((items) => items.map((item) => item.id === queueId && item.status === "queued"
            ? { ...item, status: "failed", error: "Another message is awaiting confirmation on this task. This message was not sent." } : item));
          break;
        }
        if (!queueOwner.active || queueOwner.id !== owner) {
          await queueStore.settle(queueId, owner, "unknown", "The sending view changed. Delivery has not been retried.");
          break;
        }
        const turn: ChatTurnSubmission = { ...current, messageId: claimed.messageId, text: claimed.text,
          attachments: claimed.attachments, providerInterfaceId: claimed.providerInterfaceId,
          providerModel: claimed.providerModel, reasoningEffort: claimed.reasoningEffort, serviceTier: claimed.serviceTier, endpoint: claimed.endpoint };
        const messageId = turn.messageId;
        const nowMs = Date.now();
        const startingNewTask = !turn.runId;
        const optimisticInterfaceId = runs.find((run) => run.runId === turn.runId)?.chatInterfaceId ?? "";
        const userBubble: ChatRenderMessage = {
          id: messageId,
          runId: turn.runId || undefined,
          role: "user",
          text: turn.text,
          createdAtMs: nowMs,
          direction: "in",
          interfaceId: optimisticInterfaceId || undefined,
          pending: true,
          attachments: turn.attachments.map(({ file: _file, ...item }) => item),
        };
        const workingBubble: ChatRenderMessage = {
          id: `working_${messageId}`,
          runId: turn.runId || undefined,
          role: "assistant",
          text: "",
          createdAtMs: nowMs + 1,
          direction: "out",
          eventKind: "event",
          eventData: {
            anchor_message_id: messageId,
            work_status: startingNewTask ? "submitting" : "working",
          },
          interfaceId: optimisticInterfaceId || undefined,
          pending: true,
          terminal: false,
        };
        replaceRunPending(turn.runKey, [userBubble, workingBubble]);
        let effectiveRunId = turn.runId;
        let submittedToRuntime = false;
        let bridgeAttempted = false;
        let terminalErrorReceiptObserved = false;
        let submittedAfterSeq = -1;

        try {
          const declaredTurnProvider = providerInterfaces.find((provider) => provider.id === turn.providerInterfaceId);
          const turnProvider = declaredTurnProvider
            ? turn.runId
              ? providerForExistingRun(declaredTurnProvider,
                  projectedRunsRef.current.find((run) => run.runId === turn.runId), turn)
              : providerInExecutionContext(
                executionContexts,
                turn.eeId,
                turn.workerInterfaceId,
                declaredTurnProvider,
              )
            : null;
          if (!turnProvider) {
            throw new Error(turn.runId
              ? "This task's provider binding changed or is unavailable. Refresh the task before retrying."
              : `${turn.providerInterfaceId || "provider"} is not declared in the provider registry projection`);
          }
          const turnModel = providerModels(turnProvider).find((model) => model.model === turn.providerModel);
          if (!turnModel) {
            throw new Error(`Provider interface '${turn.providerInterfaceId}' does not admit model '${turn.providerModel || "not selected"}'`);
          }
          const turnEfforts = providerReasoningEfforts(turnProvider, turnModel.model);
          if (!providerServiceTierAvailable(turnProvider, turnModel.model, turn.serviceTier ?? "")) {
            throw new Error(`Model '${turnModel.model}' does not advertise service tier '${turn.serviceTier}'. Choose an available speed.`);
          }
          if (turnEfforts.length ? !turnEfforts.includes(turn.reasoningEffort) : Boolean(turn.reasoningEffort)) {
            throw new Error(`Model '${turnModel.model}' does not admit reasoning effort '${turn.reasoningEffort}'`);
          }
          const authoredAgent = turn.runId
            ? null
            : agentPresets.find((agent) => agent.id === turn.agentPresetId) ?? null;
          if (!turn.runId && !authoredAgent) {
            throw new Error("A new task requires one exact named Agent before runtime authoring.");
          }
          if (
            !turn.runId
            && !executionContextAllowsProvider(executionContexts, turn.eeId, turn.workerInterfaceId, turn.providerInterfaceId)
          ) {
            throw new Error(`Provider interface '${turn.providerInterfaceId}' is not granted to the selected Agent template in EE '${turn.eeId}'`);
          }
          const bridgeAttachments = await Promise.all(turn.attachments.map(fileToBridgeAttachment));
          const taskCapabilities = turn.runId
            ? null
            : executionContextTaskCapabilities(
                executionContexts,
                turn.eeId,
                turn.workerInterfaceId,
                turn.applicationIds,
                turn.driverNames,
                turn.accessMode,
              );
          if (taskCapabilities?.workspaceSelectionError) throw new Error(taskCapabilities.workspaceSelectionError);
          if (!deliveryStillAllowed(owner, turn.runId, !turn.manual, turn.providerInterfaceId)) {
            await queueStore.settle(queueId, owner, turn.manual ? "failed" : "queued", turn.manual ? "The sending view or task state changed before submission. This message was not sent." : undefined);
            replaceRunPending(turn.runKey, []);
            break;
          }
          if (turn.runId) {
            const history = !turn.manual ? await loadChatRunMessages(turn.runId) : null;
            const readStatus = (await loadChatRunStatuses([turn.runId])).find((item) => item.runId === turn.runId);
            const projected = projectedRunsRef.current.find((run) => run.runId === turn.runId);
            const status = readStatus && projected && projected.lastMovementSeq > readStatus.lastMovementSeq
              ? { ...readStatus, turnOpen: projected.turnOpen, turnState: projected.turnState,
                  lastMovementSeq: projected.lastMovementSeq, updatedAtMs: projected.updatedAtMs }
              : readStatus;
            if (!status || status.archived || !projected?.writable || status.turnOpen || (!turn.manual && (!history || !queueTurnReady(claimed, status, history)))) {
              await queueStore.settle(queueId, owner, turn.manual ? "failed" : "queued", turn.manual ? "The sending view or task state changed before submission. This message was not sent." : undefined);
              replaceRunPending(turn.runKey, []);
              break;
            }
          }
          if (!deliveryStillAllowed(owner, turn.runId, !turn.manual, turn.providerInterfaceId)) {
            await queueStore.settle(queueId, owner, turn.manual ? "failed" : "queued", turn.manual ? "The sending view or task state changed before submission. This message was not sent." : undefined);
            break;
          }
          const sourceFolders = !turn.runId ? executionContexts.find((context) => context.eeId === turn.eeId)?.sourceFolders : undefined;
          const clientContext = [
            ...clientContextRef.current,
            ...(sourceFolders?.length ? [{ name: "project_source_folders", text: JSON.stringify({ primary: sourceFolders[0], folders: sourceFolders }) }] : []),
          ];
          bridgeAttempted = true;
          const submitted = await submitOperatorTurn({
            endpoint: turn.endpoint,
            messageId,
            recoverPreparedMessage: turn.recoverPreparedMessage,
            replacesMessage: turn.replacesMessage,
            runId: turn.runId || undefined,
            providerInterfaceId: turn.providerInterfaceId,
            providerModel: turn.providerModel,
            reasoningEffort: turn.reasoningEffort, serviceTier: turn.serviceTier,
            eeId: turn.eeId || undefined,
            workerInterfaceId: turn.workerInterfaceId || undefined,
            doorHostSlots: taskCapabilities?.doorHostSlots,
            externalInputs: taskCapabilities?.externalInputs,
            externalRelays: taskCapabilities?.externalRelays,
            modelOutputs: taskCapabilities?.modelOutputs,
            contextArtifactBindings: taskCapabilities?.contextArtifactBindings,
            payloadArtifactBindings: taskCapabilities?.payloadArtifactBindings,
            selectedWorkerEnvironmentKinds: taskCapabilities?.selectedWorkerEnvironmentKinds,
            selectedTools: taskCapabilities?.selectedTools,
            clientSurfaceRole: turn.clientSurfaceRole,
            clientContext: clientContext.length ? clientContext : undefined,
            continuation: turn.continuation ?? undefined,
            text: turn.text,
            attachments: bridgeAttachments,
          });
          if ("refused" in submitted.messageVerdict) {
            bridgeAttempted = false;
            throw new Error(`Message delivery refused: ${submitted.messageVerdict.refused.reason}`);
          }
          if (!("admitted" in submitted.messageVerdict) || !Number.isFinite(submitted.messageVerdict.admitted.seq)) throw new Error("Message delivery returned no admission verdict.");
          submittedToRuntime = true;
          onAdmitted?.();
          await queueStore.settle(queueId, owner, "accepted", undefined,
            "admitted" in submitted.messageVerdict ? submitted.messageVerdict.admitted.seq : undefined, submitted.runId);
          if (!queueOwner.active || queueOwner.id !== owner || !deliveryScopeCurrent()) break;
          completeSubmissionIntent(turn.runId, messageId);
          effectiveRunId = submitted.runId;
          setInFlightTurnControlTargets((current) => ({
            ...current,
            [submitted.runId]: {
              interfaceId: submitted.chatInterfaceId,
              runId: submitted.runId,
            },
          }));
          if (!turn.runId) await refreshRuns();
          else void refreshRuns();
          if (!queueOwner.active || queueOwner.id !== owner || !deliveryScopeCurrent()) break;
          if (
            accessProfile === "lab"
            && !turn.runId
            && turn.clientSurfaceRole === "task"
            && turn.eeId
          ) {
            const projects = readClientChatProjects(accessProfile);
            if (projects.some((project) => project.id === turn.eeId)) {
              saveClientChatProjects(projects.map((project) => ({
                ...project,
                chatRunIds: project.id === turn.eeId
                  ? [...project.chatRunIds.filter((runId) => runId !== submitted.runId), submitted.runId]
                  : project.chatRunIds.filter((runId) => runId !== submitted.runId),
              })), accessProfile);
            }
          }
          const submittedRunKey = chatDraftKey(submitted.runId);
          if (!turn.runId) {
            migrateDraft("", submitted.runId);
          }
          migrateSubmission(lockedRunKey, submittedRunKey);
          lockedRunKey = submittedRunKey;
          if (chatDraftKey(activeRunIdRef.current) === turn.runKey) {
            activeRunIdRef.current = submitted.runId;
            setActiveRunId(submitted.runId);
            setLoadingMessages(false);
            setContinuation(null);
          }
          replaceRunPending(submittedRunKey, [
            { ...userBubble, interfaceId: submitted.chatInterfaceId, runId: submitted.runId },
            {
              ...workingBubble,
              interfaceId: submitted.chatInterfaceId,
              runId: submitted.runId,
            },
          ]);
          if (submitted.messageRecovered) {
            setInFlightTurnControlTargets((targets) => {
              if (!(submitted.runId in targets)) return targets;
              const next = { ...targets };
              delete next[submitted.runId];
              return next;
            });
            replaceRunPending(submittedRunKey, []);
            if (turn.runKey !== submittedRunKey) deleteDraft(turn.runKey);
            await refreshMessages(submitted.runId);
            await refreshRuns();
            current = null;
            continue;
          }
          const afterSeq = "admitted" in submitted.messageVerdict
            ? submitted.messageVerdict.admitted.seq
            : -1;
          submittedAfterSeq = afterSeq;
          mergeLiveRunMessages(submitted.runId, [{
            id: messageId,
            runId: submitted.runId,
            role: "user",
            label: "operator.message",
            direction: "in",
            text: turn.text,
            createdAtMs: nowMs,
            movementSeq: afterSeq,
            artifactId: "",
            contentHash: "",
            interfaceId: submitted.chatInterfaceId,
            eventKind: "message",
            eventData: {},
            latencyMs: 0,
            attachments: turn.attachments.map(({ file: _file, ...item }) => item),
            error: false,
            terminal: true,
          }]);
          const toolCallOrigins = liveToolCallOriginIndex(messageCacheRef.current.get(submitted.runId) ?? []);
          let liveMessages: ChatRunMessage[] = [];
          const observedReply = await observeChatReply({
            accessProfile,
            knownRunId: submitted.runId,
            interfaceId: submitted.chatInterfaceId,
            afterSeq,
            cursor: submitted.outboundCursor,
            isCurrent: () => queueOwner.active && queueOwner.id === owner && deliveryScopeCurrent(),
            onArtifacts: (artifacts) => {
              if (!deliveryScopeCurrent()) return;
              for (const artifact of artifacts) {
                if (artifact.source !== "movement") continue;
                const incidentCode = eventString(artifact.event_data?.failure_code);
                if (!artifact.error || artifact.will_retry || !incidentCode || !artifact.from_interface_id) continue;
                reportClientExtensionIncident({
                  id: `movement:${artifact.movement_seq}:${artifact.artifact_id}`,
                  code: incidentCode,
                  interfaceId: turn.providerInterfaceId || artifact.from_interface_id,
                  message: artifact.text,
                  observedAtMs: artifact.recorded_at_unix_ms,
                });
              }
              if (artifacts.some((artifact) => artifact.event_kind === "rate_limits")) {
                void refreshProviderUsage();
              }
              const incoming = artifacts.map((artifact) => {
                const projected = liveArtifactMessage(
                  artifact,
                  toolCallOrigins,
                  nowMs,
                );
                return projected;
              });
              if (!incoming.length) return;
              liveMessages = mergeChatRunMessages(liveMessages, incoming);
              mergeLiveRunMessages(submitted.runId, incoming);
              const terminalArtifact = artifacts.filter(movementArtifact).reduce<OctopusOperatorChatMovementArtifact | null>((latest, artifact) => (
                isChatReplyTerminal(artifact.event_kind, artifact.event_data, artifact.terminal) && (!latest || artifact.movement_seq > latest.movement_seq)
                  ? artifact
                  : latest
              ), null);
              if (terminalArtifact?.error) terminalErrorReceiptObserved = true;
              reconcileRunReceipt(submitted.runId, artifacts);
              const turnTerminal = liveMessages.some((message) => isChatReplyTerminal(message.eventKind, message.eventData, message.terminal));
              replaceRunPending(submittedRunKey, [
                { ...userBubble, interfaceId: submitted.chatInterfaceId, runId: submitted.runId },
                ...liveMessages.map(toRender),
                ...(turnTerminal ? [] : [{
                  ...workingBubble,
                  eventData: { ...workingBubble.eventData, work_status: "working" },
                  interfaceId: submitted.chatInterfaceId,
                  runId: submitted.runId,
                }]),
              ]);
            },
          });
          if (!observedReply || !deliveryScopeCurrent()) break;
          setInFlightTurnControlTargets((current) => {
            if (!(submitted.runId in current)) return current;
            const next = { ...current };
            delete next[submitted.runId];
            return next;
          });
          replaceRunPending(submittedRunKey, []);
          if (turn.runKey !== submittedRunKey) deleteDraft(turn.runKey);
          if (turn.queueId) {
            await replaceQueuedTurns((items) => items.filter((item) => item.id !== turn.queueId));
          }
          const queued = nextQueuedTurn(submittedRunKey);
          const resumeRequired = operatorTurnResumeAvailable(liveMessages, false);
          if (queued && resumeRequired) {
            await replacePausedQueueRunKeys((current) => current.includes(submittedRunKey)
              ? current
              : [...current, submittedRunKey]);
          }
          if (queued) {
            await refreshRuns();
            await refreshMessages(submitted.runId);
          } else {
            void refreshRuns();
            void refreshMessages(submitted.runId);
          }
          current = null;
        } catch (failure) {
          const formatted = formatRuntimeError(failure);
          const notSent = !submittedToRuntime && queueSubmissionFailureStatus(failure, bridgeAttempted) === "failed";
          if (turn.manual && !submittedToRuntime && !notSent && effectiveRunId) {
            const history = await refreshMessages(effectiveRunId);
            const admitted = history?.find((message) => message.id === messageId && message.role === "user");
            if (admitted) {
              await queueStore.settle(queueId, owner, "accepted", undefined, admitted.movementSeq, effectiveRunId);
              completeSubmissionIntent(turn.runId, messageId);
              replaceRunPending(turn.runKey, []);
              current = null;
              continue;
            }
          }
          const activeKey = chatDraftKey(activeRunIdRef.current);
          const restoreManualDraft = turn.manual && !submittedToRuntime
            && (activeKey === turn.runKey || activeKey === chatDraftKey(effectiveRunId))
            && !composerDraftRef.current.text.trim() && !composerDraftRef.current.attachments.length;
          if (turn.queueId && !restoreManualDraft) {
            if (!submittedToRuntime) await queueStore.settle(queueId, owner,
              queueSubmissionFailureStatus(failure, bridgeAttempted), formatted);
            else await replacePausedQueueRunKeys((keys) => keys.includes(lockedRunKey) ? keys : [...keys, lockedRunKey]);
          } else {
            const activeKey = chatDraftKey(activeRunIdRef.current);
            if (activeKey === turn.runKey || activeKey === chatDraftKey(effectiveRunId)) {
              if (!composerDraftRef.current.text.trim() && !composerDraftRef.current.attachments.length) {
                const restored = {
                  messageId: notSent ? "" : messageId, messageHadAttachments: turn.attachments.length > 0,
                  text: turn.text, attachments: turn.attachments, continuation: turn.continuation,
                  agentPresetId: turn.agentPresetId, eeId: turn.eeId, workerInterfaceId: turn.workerInterfaceId,
                  providerInterfaceId: turn.providerInterfaceId, providerModel: turn.providerModel,
                  reasoningEffort: turn.reasoningEffort, serviceTier: turn.serviceTier,
                  applicationIds: turn.applicationIds, driverNames: turn.driverNames, accessMode: turn.accessMode,
                };
                applyDraft(restored);
                writeDraft(activeRunIdRef.current, restored);
              }
            }
          }
          if (restoreManualDraft) {
            await replaceQueuedTurns((items) => items.filter((item) => item.id !== turn.queueId));
          }
          if (
            (!turn.queueId || turn.manual)
            && (chatDraftKey(effectiveRunId) === chatDraftKey(activeRunIdRef.current) || turn.runKey === chatDraftKey(activeRunIdRef.current))
          ) {
            setError(formatted);
          }
          if (!deliveryScopeCurrent()) break;
          if (!submittedToRuntime) {
            replaceRunPending(turn.runKey, []);
            current = null;
            continue;
          }
          const errorRunId = effectiveRunId || turn.runId;
          let projectedAfterFailure: ChatRunMessage[] | null = null;
          if (errorRunId) {
            await refreshRuns();
            projectedAfterFailure = await refreshMessages(errorRunId);
          }
          if (!deliveryScopeCurrent()) break;
          const durableTerminalObserved = Boolean(projectedAfterFailure?.some((message) => (
            message.direction === "out"
            && isChatReplyTerminal(message.eventKind, message.eventData, message.terminal)
            && message.movementSeq > submittedAfterSeq
          )));
          const terminalObserved = terminalErrorReceiptObserved || durableTerminalObserved;
          if (errorRunId && terminalObserved) {
            setInFlightTurnControlTargets((targets) => {
              if (!(errorRunId in targets)) return targets;
              const next = { ...targets };
              delete next[errorRunId];
              return next;
            });
          }
          replaceRunPending(chatDraftKey(errorRunId), terminalObserved ? [] : [
            { ...userBubble, runId: errorRunId || undefined },
            chatObservationFailureMessage({
              id: `observation_${messageId}`,
              runId: errorRunId,
              detail: formatted,
              createdAtMs: Date.now(),
              afterSeq: (messageCacheRef.current.get(errorRunId) ?? []).reduce((seq, message) => Math.max(seq, message.movementSeq), submittedAfterSeq),
            }),
          ]);
          current = null;
        }
      }
    } finally {
      if (deliveryScopeCurrent()) releaseSubmission(lockedRunKey);
      else submitLockRef.current.delete(lockedRunKey);
    }
  }

  async function submit(event?: FormEvent, textOverride?: string, mode: "send" | "queue" | "steer" = "send") {
    event?.preventDefault();
    const runKey = chatDraftKey(activeRunIdRef.current);
    if (composerActionRef.current.has(runKey)) return;
    composerActionRef.current.add(runKey);
    let actionHeld = true;
    const releaseComposerAction = () => {
      if (!actionHeld) return;
      actionHeld = false;
      composerActionRef.current.delete(runKey);
    };
    try { await submitDraft(event, textOverride, mode, releaseComposerAction); }
    catch (failure) { if (deliveryScopeCurrent()) setError(formatRuntimeError(failure)); }
    finally { releaseComposerAction(); }
  }

  async function submitDraft(
    event: FormEvent | undefined, textOverride: string | undefined,
    mode: "send" | "queue" | "steer", releaseComposerAction: () => void,
  ) {
    event?.preventDefault();
    const healthBlockReason = deliveryContextRef.current.storageBlockReason || interfaceFailureReason(activeRunIdRef.current, resolvedProviderInterfaceId);
    if (healthBlockReason) {
      setError(healthBlockReason);
      return;
    }
    if (!runtimeReady) {
      setError("The runtime is unavailable. Your message has not been sent.");
      return;
    }
    if (!queueReady) { setError(queueError || "Message storage is not ready for this runtime Home."); return; }
    if (!editingQueuedTurn && mode !== "queue" && !(replyOpen && mode === "send") && queuedTurnsRef.current.some((item) => item.runKey === chatDraftKey(activeRunIdRef.current) && ["sending", "unknown"].includes(item.status))) {
      setError("A message is awaiting delivery confirmation. Check its status below."); return;
    }
    const body = (textOverride ?? text).trim();
    if (!body && attachments.length === 0) return;
    const submissionRunId = activeRunIdRef.current;
    const submissionKey = chatDraftKey(submissionRunId);
    const editedQueuedTurn = draftMessageId
      ? queuedTurnsRef.current.find((item) => (
          item.runKey === submissionKey
          && item.messageId === draftMessageId
          && item.status === "editing" && item.editOwner === queueOwner.id
        )) ?? null
      : null;
    const queuedAttachmentBytesNeedRecovery = Boolean(
      editedQueuedTurn?.hadAttachments
      && editedQueuedTurn.attachments.length === 0,
    );
    const editedQueuePayloadChanged = Boolean(
      editedQueuedTurn
      && (
        body !== editedQueuedTurn.text
        || (
          !queuedAttachmentBytesNeedRecovery
          && !sameAttachmentSelection(attachments, editedQueuedTurn.attachments)
        )
      )
    );
    const submissionMessageId = editedQueuePayloadChanged
      ? `msg_${randomId()}`
      : editedQueuedTurn?.messageId || draftMessageId || `msg_${randomId()}`;
    if (editedQueuedTurn) {
      let saved: QueuedChatTurn | undefined;
      await replaceQueuedTurns((items) => items.map((item) => {
        if (item.id !== editedQueuedTurn.id || item.status !== "editing" || item.editOwner !== queueOwner.id) return item;
        saved = { ...item, messageId: submissionMessageId, text: body, attachments: [...attachments],
          hadAttachments: attachments.length > 0, status: "queued", error: undefined,
          intent: Number.isFinite(item.queuedAfterSeq) ? undefined : item.intent,
          beforeEdit: undefined, editOwner: undefined, draftBeforeEdit: undefined };
        return saved;
      }));
      if (saved) {
        restoreQueueDraft(submissionKey, editedQueuedTurn.draftBeforeEdit);
        if ((saved as QueuedChatTurn).intent) await steerQueuedTurn(editedQueuedTurn.id);
      }
      else setError("The queued message is being edited in another view.");
      return;
    }
    const recoverDraftAttachments = Boolean(draftMessageId && draftMessageHadAttachments && attachments.length === 0);
    if (mode === "queue" || (replyOpen && mode === "send")) {
      if (!canQueue) { setError("This task cannot queue a follow-up right now."); return; }
      await enqueueCurrentTurn(body);
      return;
    }
    if (mode === "steer") {
      await submitSteeringMessage({
        attachments: [...attachments],
        fromComposer: true,
        messageId: submissionMessageId,
        recoverPreparedMessage: recoverDraftAttachments,
        runKey: submissionKey,
        text: body,
      });
      return;
    }
    if (!canSubmitOperatorTurn()) {
      setError("No local writer is available for the operator turn; open the desktop app or the dev server.");
      return;
    }
    if (!activeRunIdRef.current && !executionContextReady) {
      return;
    }
    if (!activeProvider) {
      setError(providerInterfaceError || `${providerInterfaceId || "provider"} is not declared in the provider registry projection`);
      return;
    }
    const nextSubmission: ChatTurnSubmission = {
      messageId: submissionMessageId,
      recoverPreparedMessage: recoverDraftAttachments,
      runId: submissionRunId,
      runKey: submissionKey,
      text: body,
      attachments: [...attachments],
      eeId,
      workerInterfaceId,
      providerInterfaceId: activeProvider.id,
      providerModel: activeProviderModel?.model ?? "",
      reasoningEffort,
      serviceTier,
      applicationIds: submissionRunId ? [] : applicationIds,
      driverNames: submissionRunId ? [] : driverNames,
      accessMode: submissionRunId ? "project_access" : accessMode,
      endpoint,
      continuation: submissionRunId ? null : continuation,
      clientSurfaceRole,
      agentPresetId: submissionRunId ? "" : agentPresetId,
    };
    const pausedTurns = queuedTurnsRef.current.filter((item) => item.runKey === submissionKey);
    if (pausedTurns.length > 0 && pausedQueueRunKeysRef.current.includes(submissionKey)) {
      setPendingPausedQueueSubmission({
        queuedTurnIds: pausedTurns.map((item) => item.id),
        submission: nextSubmission,
      });
      return;
    }
    await beginSubmission(nextSubmission, releaseComposerAction);
  }

  async function beginSubmission(submission: ChatTurnSubmission, releaseComposerAction?: () => void, onAdmitted?: () => void) {
    const healthBlockReason = deliveryContextRef.current.storageBlockReason || interfaceFailureReason(submission.runId, submission.providerInterfaceId);
    if (healthBlockReason) { setError(healthBlockReason); return; }
    if (!submission.queueId && submission.clientSurfaceRole === "home") {
      submission = { ...submission, ...await currentHomeProviderSelection(submission.runId) };
    }
    const queueId = submission.queueId ?? `submit_${submission.messageId}`;
    await replaceQueuedTurns((items) => items.some((item) => item.id === queueId) ? items : [...items, {
      ...submission, id: queueId, intent: "turn", status: "queued", owner: queueOwner.id, hadAttachments: submission.attachments.length > 0,
      createdAtMs: Date.now(),
    }]);
    const blocked = deliveryContextRef.current.storageBlockReason || interfaceFailureReason(submission.runId, submission.providerInterfaceId);
    if (blocked) {
      await replaceQueuedTurns((items) => items.map((item) => item.id === queueId && item.status === "queued"
        ? { ...item, status: "failed", owner: undefined, error: blocked } : item));
      setError(blocked); return;
    }
    submission = { ...submission, queueId, manual: true };
    if (!submission.replacesMessage) consumeDraft(submission.runId, submission.text, submission.attachments);
    resetInputHistory();
    // The durable intent is stored; its longer-lived submission lock owns delivery now.
    releaseComposerAction?.();
    await executeTurnSequence(submission, onAdmitted);
  }

  async function currentHomeProviderSelection(runId: string) {
    const run = (await loadChatRuns()).find((candidate) => candidate.runId === runId);
    if (!deliveryScopeCurrent() || !queueOwner.active || activeRunIdRef.current !== runId) {
      throw new Error("Home changed while reading its configuration. Your message has not been sent.");
    }
    if (!run || run.clientSurfaceRole !== "home" || !run.writable || run.archived) {
      throw new Error("Home configuration is unavailable. Your message has not been sent.");
    }
    return {
      providerInterfaceId: run.providerInterfaceId,
      providerModel: run.providerModel,
      reasoningEffort: run.reasoningEffort,
      serviceTier: run.serviceTier ?? "",
    };
  }

  async function resolvePausedQueueSubmission(resolution: "cancel" | "clear" | "send") {
    const pending = pendingPausedQueueSubmission;
    if (!pending) return;
    setPendingPausedQueueSubmission(null);
    if (resolution === "cancel") return;
    const queuedTurnIds = new Set(pending.queuedTurnIds);
    if (resolution === "clear") {
      await replaceQueuedTurns((current) => current.filter((item) => !queuedTurnIds.has(item.id) || ["sending", "accepted", "unknown"].includes(item.status)));
      await replacePausedQueueRunKeys((current) => current.filter((runKey) => runKey !== pending.submission.runKey));
      await beginSubmission(pending.submission);
      return;
    }
    await beginSubmission(pending.submission);
    const resumedRunKey = queuedTurnsRef.current.find((item) => queuedTurnIds.has(item.id))?.runKey
      ?? pending.submission.runKey;
    await replacePausedQueueRunKeys((current) => current.filter((runKey) => runKey !== resumedRunKey));

  }

  async function respondToOperatorRequest(message: ChatRenderMessage, response: ChatOperatorResponse) {
    const requestId = eventString(message.eventData?.request_id);
    const interfaceId = eventString(message.interfaceId);
    const runId = eventString(message.runId) || activeRunIdRef.current;
    if (projectedRunsRef.current.find((run) => run.runId === runId)?.archived) { setError("Restore this archived task before responding."); return; }
    if (!requestId || !interfaceId || !runId) {
      setError("The operator request has no complete run/interface provenance.");
      return;
    }
    if (message.eventData?.resolved === true) return;
    const requestKind = operatorRequestResponseKind(message);
    if (response.kind !== requestKind) {
      setError("The operator response does not match the projected request.");
      return;
    }
    const applicationApproval = message.eventData?.kind === "application";
    const uploadApproval = message.eventData?.kind === "browser_file_upload";
    const historyApproval = message.eventData?.kind === "browser_history_read";
    const boundToolApproval = applicationApproval || uploadApproval || historyApproval;
    const callId = eventString(message.eventData?.call_id);
    if (boundToolApproval && (!callId || response.kind !== "approval")) {
      const detail = "The tool approval has no exact call identity or supported response.";
      setError(detail);
      throw new Error(detail);
    }
    const toolAnswer = boundToolApproval && response.kind === "approval"
      ? operatorPermissionAnswer(message.eventData ?? {}, response.decision, interfaceId) : null;
    const body = response.kind === "approval"
      ? { label: "operator.approval.response", payload: { operator_approval_response: { request_id: requestId, ...(boundToolApproval ? { call_id: callId } : {}), ...(toolAnswer ?? { decision: response.decision }) } } }
      : response.kind === "elicitation"
        ? { label: "operator.elicitation.response", payload: { operator_elicitation_response: { request_id: requestId, answer: response.answer } } }
        : { label: "operator.user_input.response", payload: { operator_user_input_response: { request_id: requestId, answers: response.answers } } };
    try {
      setError("");
      await submitInterfaceArtifact({
        interfaceId,
        runId,
        label: body.label,
        payload: body.payload,
        timeoutMs: 45_000,
      });
      await refreshMessages(runId);
    } catch (failure) {
      setError(formatRuntimeError(failure));
      throw failure;
    }
  }

  function knownMovementSeq(runId: string) {
    return Math.max(
      projectedRunsRef.current.find((run) => run.runId === runId)?.lastMovementSeq ?? -1,
      ...(messageCacheRef.current.get(runId) ?? []).map((message) => message.movementSeq),
    );
  }

  function clearPendingTurnControl(runId: string, controlId: string) {
    setPendingTurnControls((current) => {
      if (current[runId]?.controlId !== controlId) return current;
      const next = { ...current };
      delete next[runId];
      return next;
    });
  }

  async function interruptActiveTurn() {
    const target = activeTurnControlTarget;
    if (!target) return;
    const previous = pendingTurnControlsRef.current[target.runId];
    if (previous?.action === "resume" || previous?.requestPending) return;
    const controlId = previous?.controlId ?? `control_${randomId()}`;
    const afterMovementSeq = previous?.afterMovementSeq ?? knownMovementSeq(target.runId);
    setPendingTurnControls((current) => ({ ...current, [target.runId]: { action: "stop", controlId, afterMovementSeq, requestPending: true } }));
    setError("");
    try {
      const result = await submitOperatorTurnControl({
        interfaceId: target.interfaceId,
        runId: target.runId,
        action: "stop",
        controlId,
        timeoutMs: 45_000,
      });
      if (!deliveryScopeCurrent()) return;
      if ("deliveryError" in result) {
        const { stage, message } = result.deliveryError;
        if (stage === "not-sent" || stage === "refused") {
          clearPendingTurnControl(target.runId, controlId);
          setError(`Stop ${stage === "not-sent" ? "was not sent" : "was refused"}. ${message}`);
        } else setError(`Stop is awaiting confirmation. ${message}`);
        return;
      }
      await refreshMessages(target.runId);
      await refreshRuns();
    } catch (failure) {
      if (deliveryScopeCurrent()) setError(`Stop is awaiting confirmation. ${formatRuntimeError(failure)}`);
    } finally {
      if (deliveryScopeCurrent()) setPendingTurnControls((current) => current[target.runId]?.controlId === controlId
        ? { ...current, [target.runId]: { ...current[target.runId], requestPending: false } } : current);
    }
  }

  async function resumeActiveTurn() {
    const blocked = deliveryContextRef.current.storageBlockReason || interfaceFailureReason(activeRunIdRef.current);
    if (blocked) { setError(blocked); return; }
    const run = activeRun;
    if (
      !resumeAvailable
      || !run?.writable
      || !run.chatInterfaceId
      || pendingTurnControlsRef.current[run.runId]
    ) return;
    const runKey = chatDraftKey(run.runId);
    const controlId = `control_${randomId()}`;
    const afterMovementSeq = knownMovementSeq(run.runId);
    setPendingTurnControls((current) => ({ ...current, [run.runId]: { action: "resume", controlId, afterMovementSeq } }));
    setError("");
    let bridgeAttempted = false;
    try {
      if (queuedTurnsRef.current.some((turn) => turn.runKey === runKey)) {
        await replacePausedQueueRunKeys((current) => current.includes(runKey) ? current : [...current, runKey]);
      }
      if (!deliveryScopeCurrent()) return;
      bridgeAttempted = true;
      const result = await submitOperatorTurnControl({
        interfaceId: run.chatInterfaceId,
        runId: run.runId,
        action: "resume",
        controlId,
        timeoutMs: 45_000,
      });
      if (!deliveryScopeCurrent()) return;
      if ("deliveryError" in result) {
        const { stage, message } = result.deliveryError;
        if (stage === "not-sent" || stage === "refused") {
          clearPendingTurnControl(run.runId, controlId);
          setError(`Resume ${stage === "not-sent" ? "was not sent" : "was refused"}. ${message}`);
        } else setError(`Resume is awaiting confirmation. ${message}`);
        return;
      }
      await refreshMessages(run.runId);
      await refreshRuns();
    } catch (failure) {
      if (!deliveryScopeCurrent()) return;
      if (!bridgeAttempted) clearPendingTurnControl(run.runId, controlId);
      setError(`Resume ${bridgeAttempted ? "is awaiting confirmation" : "was not sent"}. ${formatRuntimeError(failure)}`);
    }
  }

  async function reorderQueuedTurns(activeId: string, overId: string) {
    const runKey = chatDraftKey(activeRunIdRef.current);
    await replaceQueuedTurns((current) => {
      const scoped = current.filter((item) => item.runKey === runKey);
      const from = scoped.findIndex((item) => item.id === activeId);
      const to = scoped.findIndex((item) => item.id === overId);
      if (from < 0 || to < 0 || from === to || [scoped[from], scoped[to]].some((item) => !["queued", "failed"].includes(item.status))) return current;
      const next = [...scoped];
      next.splice(to, 0, next.splice(from, 1)[0]);
      let index = 0;
      return current.map((item) => item.runKey === runKey ? next[index++] : item);
    });
  }

  async function deleteQueuedTurn(id: string) {
    const runKey = chatDraftKey(activeRunIdRef.current);
    let removed: QueuedChatTurn | undefined;
    let previousId: string | undefined;
    let nextId: string | undefined;
    let index = -1;
    await replaceQueuedTurns((current) => {
      index = current.findIndex((item) => item.id === id && item.runKey === runKey);
      const item = current[index];
      if (!item || ["sending", "accepted", "editing"].includes(item.status)) return current;
      removed = item;
      const peers = current.filter((candidate) => candidate.runKey === runKey);
      const position = peers.findIndex((candidate) => candidate.id === id);
      previousId = peers[position - 1]?.id; nextId = peers[position + 1]?.id;
      return current.filter((candidate) => candidate.id !== id);
    });
    if (!removed) return;
    const saved = removed as QueuedChatTurn;
    appToast.info(saved.status === "unknown" ? "Unconfirmed message removed locally" : "Queued message deleted", 8, {
      dedupeKey: `queued-turn-delete:${id}`,
      action: { label: "Undo", onSelect: () => {
        void replaceQueuedTurns((current) => {
          if (current.some((item) => item.id === saved.id)) return current;
          const next = [...current];
          const before = next.findIndex((item) => item.id === nextId);
          const after = next.findIndex((item) => item.id === previousId);
          next.splice(before >= 0 ? before : after >= 0 ? after + 1 : Math.min(index, next.length), 0, saved);
          return next;
        }).catch((failure) => setError(formatRuntimeError(failure)));
      } },
    });
  }

  async function editQueuedTurn(id: string) {
    const owner = queueOwner.id;
    if (!deliveryScopeCurrent() || !queueOwner.active) return;
    const runId = activeRunIdRef.current;
    const runKey = chatDraftKey(runId);
    saveCurrentDraft(runId);
    const backup = readDraft(runId);
    const composerSnapshot = composerDraftRef.current;
    let original: QueuedChatTurn | undefined;
    let selected: QueuedChatTurn | undefined;
    await replaceQueuedTurns((current) => {
      if (!deliveryScopeCurrent() || !queueOwner.active || queueOwner.id !== owner) return current;
      if (current.some((item) => item.runKey === runKey && item.status === "editing" && item.id !== id)) return current;
      return current.map((item) => {
        if (item.id !== id || item.runKey !== runKey || !["queued", "failed", "editing"].includes(item.status)
          || (item.status === "queued" && item.owner && item.owner !== owner) || (item.editOwner && item.editOwner !== owner)) return item;
        original = item;
        selected = { ...item, status: "editing", editOwner: owner,
          beforeEdit: item.status === "editing" ? item.beforeEdit : { status: item.status === "failed" ? "failed" : "queued", error: item.error },
          draftBeforeEdit: item.status === "editing" ? item.draftBeforeEdit : backup, error: undefined };
        return selected;
      });
    });
    if (!selected) return;
    const latest = composerDraftRef.current;
    if (!deliveryScopeCurrent() || !queueOwner.active || queueOwner.id !== owner
      || latest.runId !== composerSnapshot.runId || latest.text !== composerSnapshot.text || !sameAttachmentSelection(latest.attachments, composerSnapshot.attachments)) {
      await replaceQueuedTurns((items) => items.map((item) => item.id === id && item.runKey === runKey && item.messageId === original?.messageId
        && item.status === "editing" && item.editOwner === owner ? original! : item));
      return;
    }
    const item = selected as QueuedChatTurn;
    const savedEdit = backup?.messageId === item.messageId ? backup : undefined;
    applyDraft({ ...(backup ?? { agentPresetId: "", applicationIds: [], driverNames: [], accessMode: "project_access", continuation: null }),
      messageId: item.messageId, messageHadAttachments: item.hadAttachments, text: savedEdit?.text ?? item.text,
      attachments: savedEdit?.attachments ?? item.attachments, eeId: item.eeId, workerInterfaceId: item.workerInterfaceId,
      providerInterfaceId: item.providerInterfaceId, providerModel: item.providerModel, reasoningEffort: item.reasoningEffort, serviceTier: item.serviceTier });
    resetInputHistory();
  }

  async function cancelQueuedEdit() {
    const owner = queueOwner.id;
    if (!deliveryScopeCurrent() || !queueOwner.active) return;
    const runKey = chatDraftKey(activeRunIdRef.current);
    let previous: ChatDraftValue | undefined;
    let cancelled = false;
    await replaceQueuedTurns((current) => current.map((item) => {
      if (!deliveryScopeCurrent() || !queueOwner.active || queueOwner.id !== owner
        || item.runKey !== runKey || item.status !== "editing" || item.editOwner !== owner) return item;
      previous = item.draftBeforeEdit; cancelled = true;
      return { ...item, status: item.beforeEdit?.status ?? "failed",
        error: item.beforeEdit ? item.beforeEdit.error : "The previous queue state is unavailable. Retry or edit this message to continue.",
        beforeEdit: undefined, editOwner: undefined, draftBeforeEdit: undefined };
    }));
    if (cancelled && deliveryScopeCurrent() && queueOwner.active && queueOwner.id === owner) restoreQueueDraft(runKey, previous);
  }
  function restoreQueueDraft(runKey: string, previous: ChatDraftValue | undefined) {
    const runId = runKey === chatDraftKey("") ? "" : runKey;
    if (previous) writeDraft(runId, previous);
    else deleteDraft(runId);
    if (chatDraftKey(activeRunIdRef.current) !== runKey) return;
    if (previous) applyDraft(previous);
    else { setDraftMessageId(""); setText(""); setAttachments([]); }
  }

  async function submitSteeringMessage(input: {
    attachments: ChatAttachment[]; fromComposer: boolean; messageId?: string;
    recoverPreparedMessage?: boolean; queueId?: string; runKey: string; text: string; editedMessageId?: string;
  }): Promise<boolean> {
    const run = activeRun;
    const blocked = deliveryContextRef.current.storageBlockReason || interfaceFailureReason(activeRunIdRef.current);
    if (blocked) { setError(blocked); return false; }
    if (!queueOwner.active || !queueReady || !enabled || !runtimeReady || !canSteer || !run
      || input.runKey !== chatDraftKey(activeRunIdRef.current) || !run.turnOpen || !run.chatInterfaceId) {
      setError("The current reply cannot accept steering. Your message has not been sent."); return false;
    }
    if (steeringComposerRef.current.has(input.runKey)) return false;
    const owner = queueOwner.id;
    const messageId = input.messageId ?? `msg_${randomId()}`;
    const queueId = input.queueId ?? `steer_${messageId}`;
    const draftBeforeSubmission = readDraft(run.runId);
    let bridgeAttempted = false;
    let accepted = false;
    steeringComposerRef.current.add(input.runKey);
    setSteeringComposerPendingRunKeys((keys) => [...new Set([...keys, input.runKey])]);
    try {
      if (!input.queueId) await replaceQueuedTurns((items) => items.some((item) => item.id === queueId) ? items : [...items, {
        id: queueId, messageId, runKey: input.runKey, text: input.text, attachments: input.attachments, clientSurfaceRole: run.clientSurfaceRole,
        hadAttachments: input.attachments.length > 0, eeId: run.eeId, workerInterfaceId: run.workerChoiceInterfaceId || run.workerInterfaceId,
        providerInterfaceId: run.providerInterfaceId, providerBoundInterfaceId: run.providerBoundInterfaceId,
        providerModel: run.providerModel, reasoningEffort,
        endpoint, createdAtMs: Date.now(), status: "queued", intent: "steer", owner,
      }]);
      const claimed = await queueStore.claim(queueId, owner, true);
      if (!claimed) {
        if (!input.queueId) await replaceQueuedTurns((items) => items.map((item) => item.id === queueId && item.status === "queued"
          ? { ...item, status: "failed", error: "Another message is awaiting confirmation. This message was not sent." } : item));
        setError("Another message is awaiting delivery confirmation on this task."); return false;
      }
      if (input.fromComposer && input.runKey === chatDraftKey(activeRunIdRef.current)) {
        consumeDraft(run.runId, claimed.text, claimed.attachments); resetInputHistory();
      }
      const bridgeAttachments = await Promise.all(claimed.attachments.map(fileToBridgeAttachment));
      const status = (await loadChatRunStatuses([run.runId])).find((item) => item.runId === run.runId);
      if (!deliveryStillAllowed(owner, run.runId) || !status?.turnOpen
        || input.editedMessageId && input.editedMessageId !== steerableMessageIdRef.current) {
        await queueStore.settle(queueId, owner, "failed", "The task stopped accepting steering before submission. The message was not sent.");
        return false;
      }
      bridgeAttempted = true;
      const submitted = await submitOperatorSteer({
        runId: run.runId, messageId, recoverPreparedMessage: input.recoverPreparedMessage,
        providerInterfaceId: run.providerInterfaceId, providerModel: run.providerModel,
        eeId: run.eeId, workerInterfaceId: run.workerInterfaceId, clientSurfaceRole: run.clientSurfaceRole,
        clientContext: clientContextRef.current.length ? clientContextRef.current : undefined,
        endpoint: claimed.endpoint, text: claimed.text, attachments: bridgeAttachments, timeoutMs: 45_000,
      });
      if ("refused" in submitted.verdict) {
        bridgeAttempted = false;
        throw new Error(`Message delivery refused: ${submitted.verdict.refused.reason}`);
      }
      if (!("admitted" in submitted.verdict) || !Number.isFinite(submitted.verdict.admitted.seq)) throw new Error("Steering returned no admission verdict.");
      accepted = true;
      await queueStore.settle(queueId, owner, "accepted", undefined, submitted.verdict.admitted.seq);
      if (!deliveryScopeCurrent()) return true;
      setPendingSteeringMessages((current) => [...current.filter((entry) => entry.message.id !== messageId), {
        runKey: input.runKey,
        message: { id: messageId, runId: run.runId, role: "user", text: claimed.text, createdAtMs: claimed.createdAtMs,
          direction: "in", interfaceId: run.chatInterfaceId, eventKind: "steering", eventData: {
            steering_status: "submitted", steering_provider_interface: run.providerBoundInterfaceId,
          },
          attachments: claimed.attachments.map(({ file: _file, stage: _stage, ...attachment }) => attachment), pending: false, terminal: true },
      }]);
      void refreshMessages(run.runId); void refreshRuns();
      return true;
    } catch (failure) {
      const formatted = formatRuntimeError(failure);
      if (!accepted) await queueStore.settle(queueId, owner, queueSubmissionFailureStatus(failure, bridgeAttempted), formatted);
      if (!accepted && input.fromComposer && draftBeforeSubmission && input.runKey === chatDraftKey(activeRunIdRef.current)
        && !composerDraftRef.current.text.trim() && !composerDraftRef.current.attachments.length) {
        const restored = { ...draftBeforeSubmission, messageId: queueSubmissionFailureStatus(failure, bridgeAttempted) === "failed" ? "" : messageId,
          messageHadAttachments: input.attachments.length > 0, text: input.text, attachments: input.attachments };
        applyDraft(restored);
        writeDraft(activeRunIdRef.current, restored);
        await replaceQueuedTurns((items) => items.filter((item) => item.id !== queueId));
      }
      if (deliveryScopeCurrent() && input.runKey === chatDraftKey(activeRunIdRef.current)) setError(formatted);
      return false;
    } finally {
      steeringComposerRef.current.delete(input.runKey);
      if (deliveryScopeCurrent()) setSteeringComposerPendingRunKeys((keys) => keys.filter((key) => key !== input.runKey));
    }
  }

  async function steerQueuedTurn(id: string) {
    const runKey = chatDraftKey(activeRunIdRef.current);
    const owner = queueOwner.id;
    if (!canSendQueued || !deliveryScopeCurrent() || !queueOwner.active) return;
    const steer = replyOpen;
    let selected: QueuedChatTurn | undefined;
    await replaceQueuedTurns((items) => items.map((item) => {
      if (!deliveryScopeCurrent() || !queueOwner.active || queueOwner.id !== owner
        || item.id !== id || item.runKey !== runKey || !["queued", "failed"].includes(item.status)
        || (item.status === "queued" && item.owner && item.owner !== owner)) return item;
      selected = { ...item, status: "queued", owner, intent: steer ? "steer" : "turn", error: undefined };
      return selected;
    }));
    if (!selected) return;
    const item = selected as QueuedChatTurn;
    if (!deliveryScopeCurrent() || !queueOwner.active || queueOwner.id !== owner || runKey !== chatDraftKey(activeRunIdRef.current)) {
      await replaceQueuedTurns((items) => items.map((candidate) => candidate.id === id && candidate.status === "queued" && candidate.owner === owner
        ? { ...candidate, status: "failed", owner: undefined, error: "The sending view changed. This message was not sent." } : candidate));
      return;
    }
    if (steer) {
      const submitted = await submitSteeringMessage({ attachments: item.attachments, fromComposer: false,
        messageId: item.messageId, queueId: item.id, runKey: item.runKey, text: item.text });
      if (!submitted) await replaceQueuedTurns((items) => items.map((candidate) => candidate.id === id && candidate.status === "queued" && candidate.owner === item.owner
        ? { ...candidate, status: "failed", owner: undefined, error: "The task stopped accepting steering. This message was not sent." } : candidate));
    }
    else {
      try { await executeTurnSequence({ ...queuedSubmission(item), manual: true }); }
      catch (failure) {
        await replaceQueuedTurns((items) => items.map((candidate) => candidate.id === id && candidate.status === "queued" && candidate.owner === owner
          ? { ...candidate, status: "failed", owner: undefined, error: formatRuntimeError(failure) } : candidate));
        throw failure;
      }
    }
  }
  async function prioritizeQueuedTurn(id: string) {
    const runKey = chatDraftKey(activeRunIdRef.current);
    await replaceQueuedTurns((current) => {
      const target = current.find((item) => item.id === id && item.runKey === runKey && item.status === "queued");
      if (!target) return current;
      const peers = [target, ...current.filter((item) => item.runKey === runKey && item.id !== id)];
      let index = 0;
      return current.map((item) => item.runKey === runKey ? peers[index++] : item);
    });
  }
  async function retryQueuedTurn(id: string) {
    if (queuedTurnsRef.current.find((item) => item.id === id)?.status !== "failed") return;
    await steerQueuedTurn(id);
  }
  async function resumeQueuedTurns() {
    const runKey = chatDraftKey(activeRunIdRef.current);
    queueAttemptedProjection.delete(runKey);
    await queueStore.change((state) => {
      const head = state.turns.find((item) => item.runKey === runKey && item.status !== "accepted");
      return { pausedRunKeys: state.pausedRunKeys.filter((key) => key !== runKey),
        turns: state.turns.map((item) => item === head && item.status === "queued" ? { ...item, resumeAfterInterrupt: true } : item) };
    });
  }

  function onComposerKeyDown(event: globalThis.KeyboardEvent, editor: HTMLElement) {
    if (event.isComposing) return;
    if (event.key === "Escape" && turnOpen) {
      event.preventDefault();
      void interruptActiveTurn();
      return;
    }
    if (navigateInputHistory(event, editor, inputHistory)) return;
    if (event.key !== "Enter" || event.isComposing) return;
    const modifier = event.metaKey || event.ctrlKey;
    const multiline = text.includes("\n");
    const shouldSend = clientPreferences.sendShortcut === "enter"
      ? !event.shiftKey
      : clientPreferences.sendShortcut === "cmd-if-multiline"
        ? modifier || (!multiline && !event.shiftKey)
        : modifier;
    if (!shouldSend) return;
    event.preventDefault();
    if (canSubmit) void submit();
  }

  const presentedRuns = inFlightRunKeys.length > 0
    ? runs.map((run) => inFlightRunKeySet.has(chatDraftKey(run.runId)) ? { ...run, turnState: "working" as const } : run)
    : runs;
  const composerAgentPresets = activeTaskAgent && !agentPresets.some((agent) => agent.id === activeTaskAgent.id)
    ? [activeTaskAgent, ...agentPresets]
    : agentPresets;

  function paneTransferBlockReason(runId: string): string {
    if (runId === activeRunIdRef.current) saveCurrentDraft(runId);
    const draft = readDraft(runId);
    if (draft?.attachments.length) return "Send or remove attached files before moving this task.";
    if (draft?.text || draft?.continuation) return "Send or clear the current draft before moving this task.";
    if (queuedTurnsRef.current.some((turn) => turn.runKey === chatDraftKey(runId))) {
      return "Send or remove queued follow-ups before moving this task.";
    }
    return "";
  }

  return {
    accessProfile,
    runtimeHome,
    storageBlockReason,
    providerFailureReason,
    runtimeBlockReason,
    accessMode,
    activeRun,
    activeRunId,
    activeMessageCount,
    activeProvider,
    agentPresetId,
    agentPresets,
    composerAgentPresets,
    artifactWriterAvailable,
    attachments,
    applicationIds,
    applicationMutationPending: Boolean(applicationMutationPending),
    applicationSelectionLocked,
    applicationSelectionLockReason,
    busy: replyOpen,
    submitting,
    canSubmit,
    canSteer,
    canSendQueued,
    clearContinuation,
    createChat,
    continueInNewTask,
    continuation,
    dragOver,
    driverNames,
    dropFiles,
    deleteQueuedTurn: (...args: Parameters<typeof deleteQueuedTurn>) => deleteQueuedTurn(...args).catch((failure) => setError(formatRuntimeError(failure))),
    editQueuedTurn: (...args: Parameters<typeof editQueuedTurn>) => editQueuedTurn(...args).catch((failure) => setError(formatRuntimeError(failure))),
    editUserMessage,
    steerUserMessage,
    steerableMessageId,
    endpoint,
    eeId,
    error: error || providerUsageError,
    executionContextError,
    fullAccessUnavailableReason,
    executionContexts,
    fileInputRef,
    interruptActiveTurn,
    loadingRuns,
    loadingMessages,
    messageQuery,
    onComposerKeyDown,
    onDragLeave,
    onDragOver,
    onFilesPicked,
    pinnedMessages,
    pinnedContextItems,
    pinningArtifactIds,
    prioritizeQueuedTurn: (...args: Parameters<typeof prioritizeQueuedTurn>) => prioritizeQueuedTurn(...args).catch((failure) => setError(formatRuntimeError(failure))),
    newTaskAuthoringError,
    providerInterfaceError,
    providerInterfaceId: resolvedProviderInterfaceId,
    providerModel,
    providerInterfacesLoaded,
    providerInterfacesRefreshing,
    declaredProviderInterfaces: agentProviderInterfaces,
    modelSelectionLoading,
    providerInterfaces: presentedProviderInterfaces,
    reasoningEffort,
    serviceTier,
    resolvePausedQueueSubmission,
    resumeActiveTurn,
    resumeAvailable,
    resumeQueuedTurns: (...args: Parameters<typeof resumeQueuedTurns>) => resumeQueuedTurns(...args).catch((failure) => setError(formatRuntimeError(failure))),
    respondToOperatorRequest,
    refresh,
    refreshProviderModels,
    refreshRuns,
    removeAttachment,
    reorderQueuedTurns: (...args: Parameters<typeof reorderQueuedTurns>) => reorderQueuedTurns(...args).catch((failure) => setError(formatRuntimeError(failure))),
    renderedMessages,
    contextUsage: latestContextUsage(messages),
    retryQueuedTurn: (...args: Parameters<typeof retryQueuedTurn>) => retryQueuedTurn(...args).catch((failure) => setError(formatRuntimeError(failure))),
    runs: presentedRuns,
    runsError,
    runsLoaded,
    runtimeReady,
    selectRun,
    setEndpoint,
    setEeId: chooseExecutionEnvironment,
    setAgentPresetId: chooseAgentPreset,
    setAccessMode: chooseTaskAccessMode,
    setMessageQuery,
    setProviderInterfaceId: chooseProviderInterface,
    setProviderModel: chooseProviderModel,
    setReasoningEffort: chooseReasoningEffort,
    chooseServiceTier,
    toggleApplication,
    toggleDriver,
    setText: setComposerText,
    submit,
    canQueue,
    editingQueuedTurn: Boolean(editingQueuedTurn),
    cancelQueuedEdit: (...args: Parameters<typeof cancelQueuedEdit>) => cancelQueuedEdit(...args).catch((failure) => setError(formatRuntimeError(failure))),
    queueCurrentTurn: () => submit(undefined, undefined, "queue").catch((failure) => setError(formatRuntimeError(failure))),
    steerCurrentTurn: () => submit(undefined, undefined, "steer").catch((failure) => setError(formatRuntimeError(failure))),
    submitBlockReason,
    steerQueuedTurn: (...args: Parameters<typeof steerQueuedTurn>) => steerQueuedTurn(...args).catch((failure) => setError(formatRuntimeError(failure))),
    startReviewTask,
    steeringWriterAvailable,
    text,
    turnOpen,
    turnControlPending: Boolean(pendingTurnControls[activeRunId] && (pendingTurnControls[activeRunId].action === "resume" || pendingTurnControls[activeRunId].requestPending)),
    toolPresentations,
    workerEnvironmentRooms,
    workerInterfaceId,
    pausedQueueSubmissionCount: pendingPausedQueueSubmission?.queuedTurnIds.length ?? 0,
    paneTransferBlockReason,
    queuePaused: pausedQueueRunKeys.includes(chatDraftKey(activeRunId)) && visibleQueuedTurns.length > 0,
    queuedTurns: visibleQueuedTurns,
    pinMessageExcerpt,
    togglePinnedMessage,
    unpinContextItem,
  };
}

export type ChatWorkspace = ReturnType<typeof useChatWorkspace>;

function projectAgents(
  agents: readonly ClientAgentDefinition[],
  projectEeId: string,
): ClientAgentDefinition[] {
  return agents.filter((agent) => agent.projectEeId === projectEeId);
}

function exactClientAgentConfiguration(
  agents: readonly ClientAgentDefinition[],
  projectEeId: string,
  workerInterfaceId: string,
  providerInterfaceId: string,
  providerModel: string,
  reasoningEffort: string,
  serviceTier: string | undefined,
): ClientAgentDefinition | null {
  if (!projectEeId || !workerInterfaceId || !providerInterfaceId) return null;
  const matches = agents.filter((agent) => (
    agent.projectEeId === projectEeId
    && agent.workerInterfaceId === workerInterfaceId
    && agent.providerInterfaceId === providerInterfaceId
    && agent.providerModel === providerModel
    && agent.reasoningEffort === reasoningEffort
    && (agent.serviceTier ?? "") === (serviceTier ?? "")
  ));
  return matches.length === 1 ? matches[0] : null;
}

function clientAgentForRun(
  agents: readonly ClientAgentDefinition[],
  run: ChatRun,
): ClientAgentDefinition | null {
  const workerInterfaceId = run.workerChoiceInterfaceId || run.workerInterfaceId;
  const projectedCandidates = agents.filter((agent) => (
    agent.projectEeId === run.eeId
    && agent.workerInterfaceId === workerInterfaceId
  ));
  return projectedCandidates.length === 1 ? projectedCandidates[0] : null;
}

function sameAttachmentSelection(
  left: readonly ChatAttachment[],
  right: readonly ChatAttachment[],
): boolean {
  return left.length === right.length
    && left.every((attachment, index) => attachment.id === right[index]?.id);
}

function clientAgentCapabilitySelectionFailure(
  agent: ClientAgentDefinition,
  worker: ChatWorkerChoice | null,
  applicationIds: readonly string[],
  driverNames: readonly string[],
  accessMode: TaskAccessMode,
): string {
  const applicationId = applicationIds.find((id) => !agent.applicationIds.includes(id));
  if (applicationId) return `Application '${applicationId}' is not provided by ${agent.name}.`;
  const driverName = driverNames.find((name) => !agent.driverNames.includes(name));
  if (driverName) return `Worker Environment '${driverName}' is not provided by ${agent.name}.`;
  return taskCapabilitySelectionFailure(worker, applicationIds, driverNames, accessMode);
}

function latestSteerableUserMessageId(messages: readonly ChatRenderMessage[], runId: string, canSteer: boolean): string {
  if (!canSteer || !runId) return "";
  const latest = [...messages].reverse().find((message) => message.role === "user" && message.eventKind !== "steering");
  return latest?.runId === runId && latest.eventKind === "message" && !latest.pending && !latest.error
    && latest.eventData?._activity_turn_anchor !== true ? latest.id : "";
}

function operatorRequestResponseKind(message: ChatRenderMessage): "approval" | "elicitation" | "user_input" | null {
  if (message.eventKind === "approval") return "approval";
  if (message.eventKind === "user_input" || eventString(message.eventData?.method) === "item/tool/requestUserInput") return "user_input";
  if (message.eventKind === "elicitation") return "elicitation";
  return null;
}
